//! What a file is, at a glance: its type's Material Icon Theme icon, in its own colours.
//!
//! The file icons are Material Icon Theme's (`assets/icons/files/`, MIT, credited in its
//! `NOTICE`), as `MonoCode` draws them beside the Tabler chrome: the colour carries a file's kind
//! at a glance in trees, tabs and tool rows (`docs/decisions/ui.md`, "The chrome's icons are
//! Tabler's, and a file's are Material's"). A type whose icon is drawn for a dark ground has the
//! theme's light variant beside it (`<name>_light.svg`), taken on a light ground.
//!
//! A file is known as Material knows it: by its whole name first (`Dockerfile`, `README.md`,
//! `package.json`), then by its extension. A language with no icon kept here is known as code
//! ([`code`]), and a file neither knows is the plain document, so an unknown type never reads
//! as a known one.

/// Declares [`FileType`] and the Material file that draws each.
macro_rules! file_types {
    ($($variant:ident => $file:literal,)+) => {
        /// A file's type, as the Material icon that marks it.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum FileType {
            $(#[doc = concat!("Material's `", $file, "`.")] $variant,)+
        }

        impl FileType {
            /// Every type, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// Its icon's name in Material Icon Theme, which names its file.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $file,)+
                }
            }

            /// Its icon's markup, for a dark ground.
            const fn dark_svg(self) -> &'static str {
                match self {
                    $(Self::$variant => include_str!(
                        concat!("../assets/icons/files/", $file, ".svg")
                    ),)+
                }
            }
        }
    };
}

file_types! {
    Audio => "audio",
    Bun => "bun",
    C => "c",
    Certificate => "certificate",
    Cmake => "cmake",
    Console => "console",
    Cpp => "cpp",
    Csharp => "csharp",
    Css => "css",
    Dart => "dart",
    Database => "database",
    Deno => "deno",
    Docker => "docker",
    Document => "document",
    Elixir => "elixir",
    Font => "font",
    Git => "git",
    Go => "go",
    Gradle => "gradle",
    H => "h",
    Haskell => "haskell",
    Hpp => "hpp",
    Html => "html",
    Image => "image",
    Java => "java",
    Javascript => "javascript",
    Json => "json",
    Key => "key",
    Kotlin => "kotlin",
    License => "license",
    Lock => "lock",
    Log => "log",
    Lua => "lua",
    Makefile => "makefile",
    Markdown => "markdown",
    Nix => "nix",
    Nodejs => "nodejs",
    Npm => "npm",
    Pdf => "pdf",
    Php => "php",
    Proto => "proto",
    Python => "python",
    React => "react",
    ReactTs => "react_ts",
    Readme => "readme",
    Ruby => "ruby",
    Rust => "rust",
    Sass => "sass",
    Settings => "settings",
    Svelte => "svelte",
    Svg => "svg",
    Swift => "swift",
    Toml => "toml",
    Tsconfig => "tsconfig",
    Typescript => "typescript",
    Video => "video",
    Vue => "vue",
    Xml => "xml",
    Yaml => "yaml",
    Zig => "zig",
    Zip => "zip",
}

impl FileType {
    /// The type of the file at `path` (a name or a whole path, either separator), if it has
    /// an icon kept here.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        let name = file_name(path);
        by_name(&name).or_else(|| by_extension(extension(&name)?))
    }

    /// Its icon's markup for a ground that is `light` or dark: the light variant where
    /// Material draws one, else the one icon.
    #[must_use]
    pub const fn svg(self, light: bool) -> &'static str {
        match (self, light) {
            (Self::Bun, true) => include_str!("../assets/icons/files/bun_light.svg"),
            (Self::Deno, true) => include_str!("../assets/icons/files/deno_light.svg"),
            (Self::Toml, true) => include_str!("../assets/icons/files/toml_light.svg"),
            _ => self.dark_svg(),
        }
    }
}

/// Whether the file at `path` is code in a language with no icon kept here, which the chrome
/// marks with its code glyph rather than the plain document.
#[must_use]
pub fn code(path: &str) -> bool {
    let name = file_name(path);
    extension(&name).is_some_and(|ext| {
        matches!(ext, "graphql" | "gql" | "ps1" | "mm" | "m" | "less" | "scala" | "ml" | "clj")
    })
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

/// The type of a file known by its whole (lowercased) name, as Material's `fileNames` know it.
fn by_name(name: &str) -> Option<FileType> {
    let base = name.split('.').next().unwrap_or(name);
    Some(match name {
        "dockerfile" | "containerfile" | ".dockerignore" | ".containerignore" | "compose.yaml"
        | "compose.yml" => FileType::Docker,
        _ if name.starts_with("dockerfile.") || name.starts_with("docker-compose.") => {
            FileType::Docker
        }
        "makefile" | "gnumakefile" | "kbuild" => FileType::Makefile,
        "cmakelists.txt" | "cmakecache.txt" | "cmakepresets.json" => FileType::Cmake,
        "justfile" | ".justfile" | ".envrc" | ".bashrc" | ".bash_profile" | ".zshrc"
        | ".zprofile" | ".zshenv" | ".zlogin" | ".profile" | "pkgbuild" | "apkbuild"
        | "pre-commit" | "commit-msg" | "pre-push" => FileType::Console,
        ".gitignore"
        | ".gitattributes"
        | ".gitmodules"
        | ".gitkeep"
        | ".keep"
        | ".gitconfig"
        | ".gitmessage"
        | ".git-blame-ignore-revs"
        | "commit_editmsg"
        | "merge_msg"
        | "git-rebase-todo" => FileType::Git,
        "package.json" | "package-lock.json" | ".nvmrc" | ".node-version" => FileType::Nodejs,
        ".npmrc" | ".npmignore" => FileType::Npm,
        "bun.lock" | "bun.lockb" | "bunfig.toml" | ".bun-version" => FileType::Bun,
        "deno.json" | "deno.jsonc" | "deno.lock" => FileType::Deno,
        "tsconfig.json" => FileType::Tsconfig,
        _ if name.starts_with("tsconfig.") && extension(name) == Some("json") => FileType::Tsconfig,
        "gradle.properties" | "gradlew" | "gradlew.bat" | "gradle-wrapper.properties" => {
            FileType::Gradle
        }
        "go.mod" | "go.sum" | "go.work" | "go.work.sum" => FileType::Go,
        "gemfile" | "gemfile.lock" | "rakefile" | "podfile" | "brewfile" | ".ruby-version" => {
            FileType::Ruby
        }
        ".editorconfig" | ".env" | ".prettierrc" | ".eslintrc" | ".clang-format"
        | ".clang-tidy" => FileType::Settings,
        _ if name.starts_with(".env.") => FileType::Settings,
        "security.md" | "security.txt" | "security" => FileType::Lock,
        _ if base == "readme" => FileType::Readme,
        _ if matches!(base, "license" | "licence" | "copying" | "copyright" | "unlicense")
            || name.starts_with("license-")
            || name.starts_with("licence-") =>
        {
            FileType::License
        }
        _ => return None,
    })
}

/// The type of a file by its (lowercased) extension, as Material's `fileExtensions` know it.
fn by_extension(ext: &str) -> Option<FileType> {
    Some(match ext {
        "rs" | "ron" => FileType::Rust,
        "ts" | "cts" | "mts" => FileType::Typescript,
        "tsx" => FileType::ReactTs,
        "jsx" => FileType::React,
        "js" | "mjs" | "cjs" | "es6" => FileType::Javascript,
        "py" | "pyi" | "pyw" | "gyp" | "gypi" | "ipy" => FileType::Python,
        "go" => FileType::Go,
        "swift" => FileType::Swift,
        "html" | "htm" | "xhtml" => FileType::Html,
        "css" => FileType::Css,
        "scss" | "sass" => FileType::Sass,
        "sh" | "bash" | "zsh" | "fish" | "nu" | "ksh" | "csh" | "tcsh" | "bat" | "cmd" | "awk" => {
            FileType::Console
        }
        "sql" | "sqlite" | "sqlite3" | "db" | "db3" | "pgsql" | "psql" | "parquet" => {
            FileType::Database
        }
        "c" | "i" => FileType::C,
        "h" => FileType::H,
        "cc" | "cpp" | "cxx" | "c++" | "cp" | "ipp" | "tpp" | "cppm" | "ixx" => FileType::Cpp,
        "hh" | "hpp" | "hxx" | "h++" | "inl" => FileType::Hpp,
        "java" | "jsp" => FileType::Java,
        "kt" | "kts" => FileType::Kotlin,
        "rb" | "erb" | "rake" | "gemspec" | "podspec" | "ru" => FileType::Ruby,
        "php" | "phtml" => FileType::Php,
        "lua" => FileType::Lua,
        "zig" | "zon" => FileType::Zig,
        "nix" => FileType::Nix,
        "cs" | "csx" => FileType::Csharp,
        "hs" | "lhs" => FileType::Haskell,
        "ex" | "exs" | "eex" | "heex" | "leex" => FileType::Elixir,
        "dart" => FileType::Dart,
        "vue" => FileType::Vue,
        "svelte" => FileType::Svelte,
        "proto" => FileType::Proto,
        "mk" => FileType::Makefile,
        "cmake" => FileType::Cmake,
        "gradle" => FileType::Gradle,
        "md" | "mdx" | "markdown" | "mdown" | "mkd" | "rst" => FileType::Markdown,
        "txt" | "text" | "rtf" => FileType::Document,
        "log" => FileType::Log,
        "json" | "jsonc" | "json5" | "jsonl" | "ndjson" | "geojson" | "webmanifest" => {
            FileType::Json
        }
        "toml" => FileType::Toml,
        "yml" | "yaml" => FileType::Yaml,
        "xml" | "plist" | "xib" | "storyboard" | "xsd" | "xsl" | "xslt" => FileType::Xml,
        "ini" | "cfg" | "conf" | "config" | "properties" | "props" | "prefs" | "env" => {
            FileType::Settings
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff"
        | "ico" | "icns" | "avif" | "jxl" => FileType::Image,
        "svg" => FileType::Svg,
        "pdf" => FileType::Pdf,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" => FileType::Zip,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "opus" | "aiff" | "aif" => FileType::Audio,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" | "mpg" | "mpeg" | "wmv" => FileType::Video,
        "lock" => FileType::Lock,
        "pem" | "key" | "pub" | "asc" | "gpg" | "p12" => FileType::Key,
        "crt" | "cer" | "cert" => FileType::Certificate,
        "woff" | "woff2" | "ttf" | "otf" | "eot" | "ttc" => FileType::Font,
        "patch" => FileType::Git,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file is known by its whole name before its extension, case aside, on either
    /// separator; one it does not know has no type and is the plain document.
    #[test]
    fn a_file_is_known_by_its_name_then_its_extension() {
        assert_eq!(FileType::of("/w/src/main.rs"), Some(FileType::Rust));
        assert_eq!(FileType::of("C:\\w\\App.TSX"), Some(FileType::ReactTs));
        assert_eq!(FileType::of("Dockerfile"), Some(FileType::Docker));
        assert_eq!(FileType::of("/w/README"), Some(FileType::Readme), "the name, no extension");
        assert_eq!(FileType::of("/w/README.md"), Some(FileType::Readme), "the name first");
        assert_eq!(FileType::of("/w/Cargo.lock"), Some(FileType::Lock));
        assert_eq!(FileType::of("/w/Cargo.toml"), Some(FileType::Toml));
        assert_eq!(FileType::of("/w/package.json"), Some(FileType::Nodejs));
        assert_eq!(FileType::of("/w/tsconfig.build.json"), Some(FileType::Tsconfig));
        assert_eq!(FileType::of("/w/.env.local"), Some(FileType::Settings));
        assert_eq!(FileType::of("/w/shot.PNG"), Some(FileType::Image));
        assert_eq!(FileType::of("/w/a.tar.gz"), Some(FileType::Zip));
        assert_eq!(FileType::of("/w/LICENSE-MIT"), Some(FileType::License));
        assert_eq!(FileType::of("/w/notes"), None);
        assert_eq!(FileType::of("/w/.notes"), None, "a dotfile is not an extension");
        assert!(code("/w/schema.graphql") && FileType::of("/w/schema.graphql").is_none());
        assert!(!code("/w/notes.unknown"));
    }

    /// A lint as a test: every icon kept under `assets/icons/files/` is some type's, so no file
    /// is shipped that nothing draws, and every type's icon is markup with a view box. A light
    /// variant is kept only beside its dark one.
    #[test]
    fn every_file_icon_is_drawn_and_none_is_left_over() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/files");
        let mut kept: Vec<String> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .filter_map(|n| n.strip_suffix(".svg").map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        kept.sort();
        let mut drawn: Vec<String> = FileType::ALL
            .iter()
            .flat_map(|t| {
                let light = (t.svg(true) != t.svg(false)).then(|| format!("{}_light", t.name()));
                std::iter::once(t.name().to_owned()).chain(light)
            })
            .collect();
        drawn.sort();
        assert_eq!(kept, drawn, "the icons kept and the icons drawn");
        let notice = include_str!("../assets/icons/files/NOTICE");
        assert!(notice.contains("_light"), "the NOTICE names the light variants");
        for t in FileType::ALL {
            for light in [false, true] {
                let svg = t.svg(light);
                assert!(svg.starts_with("<svg") && svg.contains("viewBox"), "{t:?}");
            }
        }
    }
}
