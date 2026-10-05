//! What a file is, at a glance: one of nine kinds, each an SF Symbol.
//!
//! A kind is drawn in the ink of the words beside it (`docs/decisions/ui.md`, "The chrome's
//! icons are SF Symbols"). No type is drawn in colour: a coloured gear or `M↓` was the loudest
//! thing in its row.
//!
//! A file is known by its whole name first (`Dockerfile`, `README.md`, `Cargo.lock`), then by
//! its extension. One it does not know is the plain document, so an unknown type never reads
//! as a known one.

use slopty_platform::symbols::Symbol;

/// A file's kind, as the symbol that marks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileType {
    /// Source code, a build file, a shell script.
    Code,
    /// Prose: Markdown, plain text, a log, a licence.
    Text,
    /// Structured data and settings: JSON, TOML, YAML, XML, a database.
    Data,
    /// A picture.
    Image,
    /// A PDF.
    Pdf,
    /// An archive.
    Archive,
    /// Sound.
    Audio,
    /// A film.
    Video,
    /// A lock file, a key or a certificate.
    Lock,
}

impl FileType {
    /// The kind of the file at `path` (a name or a whole path, either separator), if it is one
    /// of these.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase();
        by_name(&name).or_else(|| {
            let (base, ext) = name.rsplit_once('.')?;
            // A dotfile the names do not know is not an extension on an empty name.
            if base.is_empty() { None } else { by_extension(ext) }
        })
    }

    /// The symbol that marks it.
    #[must_use]
    pub const fn symbol(self) -> Symbol {
        match self {
            Self::Code => Symbol::ChevronLeftForwardslashChevronRight,
            Self::Text => Symbol::DocText,
            Self::Data => Symbol::Curlybraces,
            Self::Image => Symbol::Photo,
            Self::Pdf => Symbol::DocRichtext,
            Self::Archive => Symbol::DocZipper,
            Self::Audio => Symbol::Waveform,
            Self::Video => Symbol::Film,
            Self::Lock => Symbol::LockDoc,
        }
    }
}

/// The kind of a file known by its whole (lowercased) name.
fn by_name(name: &str) -> Option<FileType> {
    let base = name.split('.').next().unwrap_or(name);
    Some(match name {
        "dockerfile" | "containerfile" | "makefile" | "gnumakefile" | "justfile" | ".justfile" => {
            FileType::Code
        }
        "compose.yaml"
        | "compose.yml"
        | "docker-compose.yml"
        | "docker-compose.yaml"
        | ".dockerignore"
        | ".gitignore"
        | ".gitattributes"
        | ".gitmodules"
        | ".gitkeep"
        | ".env"
        | ".envrc"
        | ".editorconfig"
        | ".npmrc"
        | ".prettierrc"
        | ".eslintrc" => FileType::Data,
        _ if name.starts_with(".env.") => FileType::Data,
        _ if matches!(
            base,
            "readme"
                | "license"
                | "licence"
                | "copying"
                | "unlicense"
                | "changelog"
                | "changes"
                | "history"
        ) =>
        {
            FileType::Text
        }
        _ => return None,
    })
}

/// The kind of a file by its (lowercased) extension.
fn by_extension(ext: &str) -> Option<FileType> {
    Some(match ext {
        "rs" | "ts" | "mts" | "cts" | "tsx" | "js" | "mjs" | "cjs" | "jsx" | "py" | "pyi"
        | "pyw" | "go" | "swift" | "html" | "htm" | "css" | "scss" | "sass" | "sh" | "bash"
        | "zsh" | "fish" | "nu" | "ps1" | "sql" | "c" | "cc" | "cpp" | "cxx" | "c++" | "hpp"
        | "hh" | "hxx" | "mm" | "h" | "java" | "kt" | "kts" | "rb" | "php" | "lua" | "zig"
        | "nix" | "cs" | "hs" | "ex" | "exs" | "dart" | "vue" | "svelte" | "graphql" | "gql"
        | "proto" | "mk" => FileType::Code,
        "md" | "mdx" | "markdown" | "txt" | "rtf" | "text" | "log" => FileType::Text,
        "json" | "jsonc" | "json5" | "jsonl" | "ndjson" | "toml" | "yml" | "yaml" | "xml"
        | "plist" | "xib" | "storyboard" | "ini" | "cfg" | "conf" | "properties" | "env"
        | "sqlite" | "sqlite3" | "db" => FileType::Data,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff"
        | "ico" | "icns" | "avif" | "jxl" | "svg" => FileType::Image,
        "pdf" => FileType::Pdf,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" => FileType::Archive,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "opus" | "aiff" => FileType::Audio,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => FileType::Video,
        "lock" | "lockb" | "pem" | "crt" | "cer" | "key" | "p12" => FileType::Lock,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file is known by its whole name before its extension, case aside, on either
    /// separator; one it does not know has no kind and is the plain document.
    #[test]
    fn a_file_is_known_by_its_name_then_its_extension() {
        assert_eq!(FileType::of("/w/src/main.rs"), Some(FileType::Code));
        assert_eq!(FileType::of("C:\\w\\App.TSX"), Some(FileType::Code));
        assert_eq!(FileType::of("Dockerfile"), Some(FileType::Code));
        assert_eq!(FileType::of("/w/README"), Some(FileType::Text), "the name, no extension");
        assert_eq!(FileType::of("/w/Cargo.lock"), Some(FileType::Lock));
        assert_eq!(FileType::of("/w/Cargo.toml"), Some(FileType::Data));
        assert_eq!(FileType::of("/w/.env.local"), Some(FileType::Data));
        assert_eq!(FileType::of("/w/shot.PNG"), Some(FileType::Image));
        assert_eq!(FileType::of("/w/a.tar.gz"), Some(FileType::Archive));
        assert_eq!(FileType::of("/w/LICENSE-MIT"), None, "a name it does not know");
        assert_eq!(FileType::of("/w/notes"), None);
        assert_eq!(FileType::of("/w/font.woff2"), None);
    }

    /// The nine kinds are nine symbols.
    #[test]
    fn each_kind_is_its_own_symbol() {
        let kinds = [
            FileType::Code,
            FileType::Text,
            FileType::Data,
            FileType::Image,
            FileType::Pdf,
            FileType::Archive,
            FileType::Audio,
            FileType::Video,
            FileType::Lock,
        ];
        let symbols: std::collections::HashSet<_> = kinds.iter().map(|k| k.symbol()).collect();
        assert_eq!(symbols.len(), kinds.len());
        assert!(!symbols.contains(&Symbol::Doc), "the plain document is for no kind");
    }
}
