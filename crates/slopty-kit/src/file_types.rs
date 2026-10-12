//! What a file is, at a glance: one of a handful of Octicons kinds, drawn in the ink beside it
//! as every glyph is (`docs/decisions/ui.md`, "The chrome's icons are Octicons at 14").
//!
//! A language's logo said no more than the name's extension beside it, and a filled logo
//! beside an outline glyph read as a second weight in the row, so a file wears its kind alone:
//! code, Markdown, a picture or a sound, an archive, a binary, else the plain document. A file
//! is known by its whole name first (`Dockerfile`, `README.md`), then by its extension.

use crate::icons::Symbol;

/// The glyph a file's kind wears: [`Symbol::FileCode`], [`Symbol::Markdown`],
/// [`Symbol::FileMedia`], [`Symbol::FileZip`], [`Symbol::FileBinary`], else [`Symbol::Doc`].
#[must_use]
pub fn glyph(path: &str) -> Symbol {
    let name = file_name(path);
    by_name(&name).or_else(|| by_extension(extension(&name)?)).unwrap_or(Symbol::Doc)
}

/// The last component of `path`, lowercased.
fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase()
}

/// The extension of a (lowercased) file name; a dotfile the names do not know has none, as
/// it is not an extension on an empty name.
fn extension(name: &str) -> Option<&str> {
    let (base, ext) = name.rsplit_once('.')?;
    if base.is_empty() { None } else { Some(ext) }
}

/// The kind of a file known by its whole (lowercased) name.
fn by_name(name: &str) -> Option<Symbol> {
    let base = name.split('.').next().unwrap_or(name);
    Some(match name {
        "dockerfile" | "containerfile" | "makefile" | "gnumakefile" | "kbuild" | "justfile"
        | ".justfile" | "cmakelists.txt" | "gemfile" | "rakefile" | "podfile" | "brewfile"
        | "pkgbuild" | "apkbuild" | "gradlew" => Symbol::FileCode,
        _ if name.starts_with("dockerfile.") => Symbol::FileCode,
        _ if base == "readme" && extension(name).is_none_or(|ext| MARKDOWN.contains(&ext)) => {
            Symbol::Markdown
        }
        _ => return None,
    })
}

/// The extensions of Markdown and its kin.
const MARKDOWN: [&str; 6] = ["md", "mdx", "markdown", "mdown", "mkd", "rst"];

/// The kind of a file by its (lowercased) extension.
fn by_extension(ext: &str) -> Option<Symbol> {
    if MARKDOWN.contains(&ext) {
        return Some(Symbol::Markdown);
    }
    Some(match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff"
        | "ico" | "icns" | "avif" | "jxl" | "svg" | "mp3" | "wav" | "flac" | "aac" | "ogg"
        | "m4a" | "opus" | "aiff" | "aif" | "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v"
        | "mpg" | "mpeg" | "wmv" => Symbol::FileMedia,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "dmg" => {
            Symbol::FileZip
        }
        "pdf" | "woff" | "woff2" | "ttf" | "otf" | "eot" | "ttc" | "sqlite" | "sqlite3" | "db"
        | "db3" | "parquet" | "bin" | "exe" | "dll" | "so" | "dylib" | "a" | "o" | "wasm"
        | "p12" => Symbol::FileBinary,
        "txt" | "text" | "rtf" | "log" => Symbol::Doc,
        "rs" | "ron" | "ts" | "cts" | "mts" | "tsx" | "jsx" | "js" | "mjs" | "cjs" | "es6"
        | "py" | "pyi" | "pyw" | "go" | "swift" | "html" | "htm" | "xhtml" | "css" | "scss"
        | "sass" | "less" | "sh" | "bash" | "zsh" | "fish" | "nu" | "ksh" | "csh" | "tcsh"
        | "bat" | "cmd" | "ps1" | "awk" | "sql" | "pgsql" | "psql" | "c" | "i" | "h" | "cc"
        | "cpp" | "cxx" | "c++" | "cp" | "ipp" | "tpp" | "cppm" | "ixx" | "hh" | "hpp" | "hxx"
        | "h++" | "inl" | "m" | "mm" | "java" | "jsp" | "kt" | "kts" | "rb" | "erb" | "rake"
        | "gemspec" | "podspec" | "php" | "phtml" | "lua" | "zig" | "zon" | "nix" | "cs"
        | "csx" | "hs" | "lhs" | "ex" | "exs" | "eex" | "heex" | "leex" | "vue" | "svelte"
        | "mk" | "gradle" | "cmake" | "scala" | "ml" | "clj" | "dart" | "proto" | "graphql"
        | "gql" | "json" | "jsonc" | "json5" | "jsonl" | "ndjson" | "geojson" | "webmanifest"
        | "toml" | "yml" | "yaml" | "xml" | "plist" | "xib" | "storyboard" | "xsd" | "xsl"
        | "xslt" | "ini" | "cfg" | "conf" | "config" | "properties" | "props" | "env" | "lock"
        | "patch" | "diff" => Symbol::FileCode,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file is known by its whole name before its extension, case aside, on either
    /// separator, as one of a handful of kinds; one it does not know is the plain document.
    #[test]
    fn a_file_leads_with_its_kinds_glyph() {
        assert_eq!(glyph("/w/src/main.rs"), Symbol::FileCode);
        assert_eq!(glyph("C:\\w\\App.TSX"), Symbol::FileCode);
        assert_eq!(glyph("Dockerfile"), Symbol::FileCode);
        assert_eq!(glyph("/w/CMakeLists.txt"), Symbol::FileCode, "code, though it ends in .txt");
        assert_eq!(glyph("/w/Cargo.toml"), Symbol::FileCode);
        assert_eq!(glyph("/w/README"), Symbol::Markdown, "the name, no extension");
        assert_eq!(glyph("/w/README.md"), Symbol::Markdown);
        assert_eq!(glyph("/w/NOTES.md"), Symbol::Markdown);
        assert_eq!(glyph("/w/shot.PNG"), Symbol::FileMedia);
        assert_eq!(glyph("/w/a.tar.gz"), Symbol::FileZip);
        assert_eq!(glyph("/w/font.woff2"), Symbol::FileBinary);
        assert_eq!(glyph("/w/build.log"), Symbol::Doc);
        assert_eq!(glyph("/w/LICENSE"), Symbol::Doc);
        assert_eq!(glyph("/w/notes"), Symbol::Doc);
        assert_eq!(glyph("/w/.notes"), Symbol::Doc, "a dotfile is not an extension");
    }
}
