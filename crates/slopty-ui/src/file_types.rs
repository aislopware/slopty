//! What a file is, at a glance: Material Icon Theme's file-type drawings (MIT,
//! `assets/file-types/LICENSE`), in their own colours.
//!
//! A file is known by its whole name first (`Dockerfile`, `README.md`, `Cargo.lock`), then by
//! its extension. One the set has no drawing for keeps the plain file icon, so an unknown type
//! never reads as a known one.

use gpui::SharedString;

/// A file's type, as the drawing that marks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileType(&'static str);

/// Where the drawings are served from ([`crate::icons::Assets`]).
const DIR: &str = "file-types/";

macro_rules! drawings {
    ($($stem:literal),* $(,)?) => {
        /// Each drawing's stem with its bytes.
        const DRAWINGS: &[(&str, &[u8])] = &[$((
            $stem,
            include_bytes!(concat!("../assets/file-types/", $stem, ".svg")),
        )),*];
    };
}

drawings![
    "audio",
    "c",
    "certificate",
    "changelog",
    "console",
    "cpp",
    "csharp",
    "css",
    "dart",
    "database",
    "docker",
    "document",
    "elixir",
    "font",
    "git",
    "go",
    "graphql",
    "h",
    "haskell",
    "html",
    "image",
    "java",
    "javascript",
    "json",
    "kotlin",
    "license",
    "lock",
    "log",
    "lua",
    "makefile",
    "markdown",
    "nix",
    "pdf",
    "php",
    "proto",
    "python",
    "react",
    "react_ts",
    "readme",
    "ruby",
    "rust",
    "sass",
    "settings",
    "svelte",
    "svg",
    "swift",
    "toml",
    "tune",
    "typescript",
    "video",
    "vue",
    "xml",
    "yaml",
    "zig",
    "zip",
];

impl FileType {
    /// The type of the file at `path` (a name or a whole path, either separator), if the set
    /// draws it.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase();
        let stem = by_name(&name).or_else(|| {
            let (base, ext) = name.rsplit_once('.')?;
            // A dotfile the names do not know is not an extension on an empty name.
            if base.is_empty() { None } else { by_extension(ext) }
        })?;
        DRAWINGS.iter().find(|(s, _)| *s == stem).map(|(s, _)| Self(s))
    }

    /// The path [`crate::icons::Assets`] serves its drawing under.
    #[must_use]
    pub fn path(self) -> SharedString {
        SharedString::from(format!("{DIR}{}.svg", self.0))
    }

    /// The drawing's name: `rust`, `typescript`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.0
    }
}

/// The drawing served under `path`, if it is one of these.
pub(crate) fn load(path: &str) -> Option<&'static [u8]> {
    let stem = path.strip_prefix(DIR)?.strip_suffix(".svg")?;
    DRAWINGS.iter().find(|(s, _)| *s == stem).map(|(_, bytes)| *bytes)
}

/// Every path served, for [`gpui::AssetSource::list`].
pub(crate) fn paths() -> impl Iterator<Item = SharedString> {
    DRAWINGS.iter().map(|(stem, _)| SharedString::from(format!("{DIR}{stem}.svg")))
}

/// A drawing for a file known by its whole (lowercased) name.
fn by_name(name: &str) -> Option<&'static str> {
    let base = name.split('.').next().unwrap_or(name);
    Some(match name {
        "dockerfile"
        | "containerfile"
        | "compose.yaml"
        | "compose.yml"
        | "docker-compose.yml"
        | "docker-compose.yaml"
        | ".dockerignore" => "docker",
        "makefile" | "gnumakefile" | "justfile" | ".justfile" => "makefile",
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => "git",
        ".env" | ".envrc" => "tune",
        ".editorconfig" | ".npmrc" | ".prettierrc" | ".eslintrc" => "settings",
        _ if name.starts_with(".env.") => "tune",
        _ if base == "readme" => "readme",
        _ if matches!(base, "license" | "licence" | "copying" | "unlicense") => "license",
        _ if matches!(base, "changelog" | "changes" | "history") => "changelog",
        _ => return None,
    })
}

/// A drawing for a file by its (lowercased) extension.
fn by_extension(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "react_ts",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "react",
        "json" | "jsonc" | "json5" | "jsonl" | "ndjson" => "json",
        "md" | "mdx" | "markdown" => "markdown",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "py" | "pyi" | "pyw" => "python",
        "go" => "go",
        "swift" => "swift",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "sass",
        "sh" | "bash" | "zsh" | "fish" | "nu" | "ps1" => "console",
        "lock" | "lockb" => "lock",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff"
        | "ico" | "icns" | "avif" | "jxl" => "image",
        "svg" => "svg",
        "pdf" => "pdf",
        "sql" | "sqlite" | "sqlite3" | "db" => "database",
        "c" => "c",
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "mm" => "cpp",
        "h" => "h",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "zig" => "zig",
        "nix" => "nix",
        "xml" | "plist" | "xib" | "storyboard" => "xml",
        "cs" => "csharp",
        "hs" => "haskell",
        "ex" | "exs" => "elixir",
        "dart" => "dart",
        "vue" => "vue",
        "svelte" => "svelte",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "log" => "log",
        "txt" | "rtf" | "text" => "document",
        "ini" | "cfg" | "conf" | "properties" => "settings",
        "env" => "tune",
        "pem" | "crt" | "cer" | "key" | "p12" => "certificate",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" => "zip",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "video",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "opus" | "aiff" => "audio",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "mk" => "makefile",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file is known by its whole name before its extension, case aside, on either
    /// separator; one the set does not draw is not given a type.
    #[test]
    fn a_file_is_known_by_its_name_then_its_extension() {
        let named = |path: &str| FileType::of(path).map(FileType::name);
        assert_eq!(named("/w/src/main.rs"), Some("rust"));
        assert_eq!(named("C:\\w\\App.TSX"), Some("react_ts"));
        assert_eq!(named("Dockerfile"), Some("docker"));
        assert_eq!(named("/w/README.md"), Some("readme"), "the name before the extension");
        assert_eq!(named("/w/Cargo.lock"), Some("lock"));
        assert_eq!(named("/w/.gitignore"), Some("git"));
        assert_eq!(named("/w/.env.local"), Some("tune"));
        assert_eq!(named("/w/LICENSE-MIT"), None, "a name it does not know");
        assert_eq!(named("/w/notes"), None);
        assert_eq!(named("/w/data.unknownext"), None);
    }

    /// Every type a name or extension can give has its drawing, and the drawing is served
    /// under its path as an SVG in its own colours.
    #[test]
    fn every_type_is_drawn_and_served() {
        let probes = [
            "a.rs",
            "a.ts",
            "a.tsx",
            "a.js",
            "a.jsx",
            "a.json",
            "a.md",
            "a.toml",
            "a.yaml",
            "a.py",
            "a.go",
            "a.swift",
            "a.html",
            "a.css",
            "a.scss",
            "a.sh",
            "a.lock",
            "a.png",
            "a.svg",
            "a.pdf",
            "a.sql",
            "a.c",
            "a.cpp",
            "a.h",
            "a.java",
            "a.kt",
            "a.rb",
            "a.php",
            "a.lua",
            "a.zig",
            "a.nix",
            "a.xml",
            "a.cs",
            "a.hs",
            "a.ex",
            "a.dart",
            "a.vue",
            "a.svelte",
            "a.graphql",
            "a.proto",
            "a.log",
            "a.txt",
            "a.ini",
            "a.env",
            "a.pem",
            "a.zip",
            "a.mp4",
            "a.mp3",
            "a.ttf",
            "a.mk",
            "Dockerfile",
            "Makefile",
            ".gitignore",
            "README",
            "LICENSE",
            "CHANGELOG.md",
            ".editorconfig",
        ];
        for probe in probes {
            let kind = FileType::of(probe).unwrap_or_else(|| panic!("{probe} has a type"));
            let bytes = load(&kind.path()).unwrap_or_else(|| panic!("{probe}'s drawing is served"));
            let text = std::str::from_utf8(bytes).unwrap_or_default();
            assert!(text.starts_with("<svg"), "{probe}: {text:.40}");
            assert!(text.contains("=\"#"), "{probe} keeps its own colours");
        }
        assert!(load("file-types/not-a-type.svg").is_none());
        assert_eq!(paths().count(), DRAWINGS.len());
    }
}
