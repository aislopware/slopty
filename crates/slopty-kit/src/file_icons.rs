// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! File and folder icons: Symbols by Miguel Solorio (MIT, `assets/file-icons/LICENSE.symbols`),
//! polychrome.
//!
//! Resolution keeps the VS Code theme's rules: an exact name wins, then the longest compound
//! extension, a language or MIME hint, and the generic file or folder last. Light keeps the
//! authored colours; dark lifts the palette's darker accents.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::{App, AssetSource, Hsla, Img, RenderImage, Result, SharedString, Styled as _, img};
use parking_lot::Mutex;
use serde::Deserialize;

use crate::syntax::LanguageId;
use crate::theme::{Appearance, Theme};

const ASSET_PREFIX: &str = "file-icons/";

fn dark_icon_svg(svg: &str) -> String {
    let mut svg = svg.to_owned();
    for (original, brighter) in [
        ("#64748B", "#CBD5E1"),
        ("#71717A", "#D4D4D8"),
        ("#2563EB", "#60A5FA"),
        ("#EA580C", "#FB923C"),
        ("#16A34A", "#4ADE80"),
        ("#8B5CF6", "#A78BFA"),
        ("#A855F7", "#C084FC"),
    ] {
        svg = svg.replace(original, brighter).replace(&original.to_ascii_lowercase(), brighter);
    }
    svg
}

/// A neutral seat for a polychrome icon, visible in both appearances without tinting it.
#[must_use]
pub fn well_bg(theme: &Theme) -> Hsla {
    let alpha = if theme.is_frost() { 0.32 } else { 0.16 };
    match theme.appearance {
        Appearance::Dark => crate::theme::grey(0).opacity(alpha),
        Appearance::Light => crate::theme::grey(255).opacity(alpha),
    }
}

#[expect(
    clippy::same_name_method,
    reason = "rust-embed's derive gives the type `get` and `iter` both inherent and by trait"
)]
mod embedded {
    use rust_embed::RustEmbed;

    #[derive(RustEmbed)]
    #[folder = "assets/file-icons"]
    #[include = "*.svg"]
    pub(super) struct EmbeddedFileIcons;
}
use embedded::EmbeddedFileIcons;

/// The file icons' half of the app's asset source.
#[derive(Debug, Clone, Copy)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let Some(path) = path.strip_prefix(ASSET_PREFIX) else {
            return Ok(None);
        };
        if let Some(path) = path.strip_prefix("dark/") {
            return EmbeddedFileIcons::get(path)
                .map(|asset| {
                    let svg = std::str::from_utf8(&asset.data)?;
                    Ok(Cow::Owned(dark_icon_svg(svg).into_bytes()))
                })
                .transpose();
        }
        Ok(EmbeddedFileIcons::get(path).map(|asset| asset.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(EmbeddedFileIcons::iter()
            .map(|asset| format!("{ASSET_PREFIX}{asset}"))
            .filter(|asset| asset.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIconKind {
    /// A file.
    File,
    /// A folder.
    Directory,
    /// A link, resolved by its name.
    Symlink,
}

/// What an icon is resolved from.
#[derive(Debug, Clone, Copy)]
pub struct FileIconIdentity<'a> {
    /// File, folder or link.
    pub kind: FileIconKind,
    /// Its path or name.
    pub name: &'a str,
    /// Whether a folder is open.
    pub expanded: bool,
    /// A language hint.
    pub language: Option<LanguageId>,
    /// A MIME hint.
    pub mime_type: Option<&'a str>,
}

impl<'a> FileIconIdentity<'a> {
    /// A file at `path`.
    #[must_use]
    pub const fn file(path: &'a str) -> Self {
        Self {
            kind: FileIconKind::File,
            name: path,
            expanded: false,
            language: None,
            mime_type: None,
        }
    }

    /// A folder at `path`.
    #[must_use]
    pub const fn directory(path: &'a str, expanded: bool) -> Self {
        Self {
            kind: FileIconKind::Directory,
            name: path,
            expanded,
            language: None,
            mime_type: None,
        }
    }

    /// A link at `path`.
    #[must_use]
    pub const fn symlink(path: &'a str) -> Self {
        Self {
            kind: FileIconKind::Symlink,
            name: path,
            expanded: false,
            language: None,
            mime_type: None,
        }
    }

    /// With a language hint.
    #[must_use]
    pub const fn with_language(mut self, language: LanguageId) -> Self {
        self.language = Some(language);
        self
    }

    /// With a MIME hint.
    #[must_use]
    pub const fn with_mime_type(mut self, mime_type: &'a str) -> Self {
        self.mime_type = Some(mime_type);
        self
    }
}

#[derive(Debug, Default, Deserialize)]
struct Manifest {
    #[serde(rename = "iconDefinitions")]
    icon_definitions: HashMap<String, IconDefinition>,
    file: Option<String>,
    folder: Option<String>,
    #[serde(default, rename = "fileExtensions")]
    file_extensions: HashMap<String, String>,
    #[serde(default, rename = "fileNames")]
    file_names: HashMap<String, String>,
    #[serde(default, rename = "folderNames")]
    folder_names: HashMap<String, String>,
    #[serde(default, rename = "languageIds")]
    language_ids: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct IconDefinition {
    #[serde(rename = "iconPath")]
    icon_path: String,
}

static MANIFEST: LazyLock<Manifest> = LazyLock::new(|| {
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../assets/file-icons/manifest.json"))
            .unwrap_or_default();
    manifest.file_extensions = lowercase_keys(std::mem::take(&mut manifest.file_extensions));
    manifest.file_names = lowercase_keys(std::mem::take(&mut manifest.file_names));
    manifest.folder_names = lowercase_keys(std::mem::take(&mut manifest.folder_names));
    manifest
});

fn lowercase_keys(values: HashMap<String, String>) -> HashMap<String, String> {
    values.into_iter().map(|(key, value)| (key.to_ascii_lowercase(), value)).collect()
}

/// The asset path an identity resolves to.
#[must_use]
pub fn asset_path(identity: FileIconIdentity<'_>, appearance: Appearance) -> SharedString {
    let name = basename(identity.name).to_ascii_lowercase();
    let asset = match identity.kind {
        FileIconKind::Directory => resolve_directory(&name),
        FileIconKind::File | FileIconKind::Symlink => resolve_file(&name, identity),
    };
    let variant = if appearance == Appearance::Dark { "dark/" } else { "" };
    SharedString::from(format!("{ASSET_PREFIX}{variant}{asset}"))
}

/// A polychrome file icon: [`img`] keeps the authored fills, where an [`gpui::Svg`] is a mask.
#[must_use]
pub fn icon(identity: FileIconIdentity<'_>, appearance: Appearance) -> Img {
    img(asset_path(identity, appearance)).flex_none()
}

/// A file icon rasterised once, for painting into a text layout where no element can sit.
#[must_use]
pub fn raster(path: &SharedString, cx: &App) -> Option<Arc<RenderImage>> {
    static CACHE: LazyLock<Mutex<HashMap<SharedString, Arc<RenderImage>>>> =
        LazyLock::new(Default::default);
    if let Some(image) = CACHE.lock().get(path) {
        return Some(Arc::clone(image));
    }
    let svg = Assets.load(path).ok()??;
    let image = cx.svg_renderer().render_single_frame(&svg, 2.0).ok()?;
    CACHE.lock().insert(path.clone(), Arc::clone(&image));
    Some(image)
}

/// Whether a name resolves to an icon of its own rather than the generic document: stricter
/// than [`asset_path`], so prose decorates a filename but not every dotted word.
#[must_use]
pub fn has_specific_file_icon(path: &str) -> bool {
    let name = basename(path).to_ascii_lowercase();
    resolve_file(&name, FileIconIdentity::file(path)) != generic_file_asset()
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn definition_asset(definition: &str) -> Option<&'static str> {
    // Two names the manifest uses without defining; its language table shows what they meant.
    let definition = match definition {
        "less" => "brackets-sky",
        "yml" => "yaml",
        definition => definition,
    };
    MANIFEST.icon_definitions.get(definition)?.icon_path.strip_prefix("./icons/")
}

fn resolve_directory(name: &str) -> &'static str {
    MANIFEST
        .folder_names
        .get(name)
        .or(MANIFEST.folder.as_ref())
        .and_then(|definition| definition_asset(definition))
        .unwrap_or("folders/folder.svg")
}

fn resolve_file(name: &str, identity: FileIconIdentity<'_>) -> &'static str {
    if let Some(asset) = MANIFEST.file_names.get(name).and_then(|d| definition_asset(d)) {
        return asset;
    }
    for extension in compound_extensions(name) {
        if let Some(asset) =
            MANIFEST.file_extensions.get(extension).and_then(|d| definition_asset(d))
        {
            return asset;
        }
    }
    language_asset(identity.language)
        .or_else(|| mime_asset(identity.mime_type))
        .unwrap_or_else(generic_file_asset)
}

fn generic_file_asset() -> &'static str {
    MANIFEST.file.as_deref().and_then(definition_asset).unwrap_or("files/document.svg")
}

fn compound_extensions(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('.')
        .filter_map(|(index, _)| name.get(index.saturating_add(1)..))
        .filter(|extension| !extension.is_empty())
}

/// The VS Code language id of a grammar, by its name.
fn language_asset(language: Option<LanguageId>) -> Option<&'static str> {
    let language_id = match language?.name() {
        "Rust" => "rust",
        "JavaScript" | "JavaScript (Babel)" => "javascript",
        "JSX" => "javascriptreact",
        "TypeScript" => "typescript",
        "TypeScriptReact" => "typescriptreact",
        "Python" => "python",
        "Go" => "go",
        "JSON" => "json",
        "Bourne Again Shell (bash)" | "Shell-Unix-Generic" => "shellscript",
        "TOML" => return definition_asset("gear"),
        "Markdown" => "markdown",
        "HTML" => "html",
        "CSS" => "css",
        "YAML" => "yaml",
        "C" => "c",
        "C++" => "cpp",
        "C#" => "csharp",
        "Java" => "java",
        "Kotlin" => return definition_asset("kotlin"),
        "Swift" => "swift",
        "Ruby" => "ruby",
        "PHP" => "php",
        "SQL" => "sql",
        "Lua" => "lua",
        "Dockerfile" => "dockerfile",
        "Nix" => "nix",
        "Makefile" => "makefile",
        _ => return None,
    };
    MANIFEST.language_ids.get(language_id).and_then(|definition| definition_asset(definition))
}

fn mime_asset(mime_type: Option<&str>) -> Option<&'static str> {
    let mime_type = mime_type?.split(';').next()?.trim().to_ascii_lowercase();
    definition_asset(match mime_type.as_str() {
        mime if mime.starts_with("image/") => "image",
        mime if mime.starts_with("audio/") => "audio",
        mime if mime.starts_with("video/") => "video",
        "application/json" | "application/ld+json" => "brackets-yellow",
        "application/pdf" => "pdf",
        "application/zip" | "application/gzip" | "application/x-tar" => "compressed",
        "text/markdown" => "markdown",
        "text/css" => "brackets-sky",
        "text/html" => "brackets-orange",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(identity: FileIconIdentity<'_>) -> SharedString {
        asset_path(identity, Appearance::Light)
    }

    #[test]
    fn dark_icons_are_their_own_brighter_assets() {
        for name in ["main.ts", "main.rs", "config.toml", "notes.txt"] {
            let light = asset_path(FileIconIdentity::file(name), Appearance::Light);
            let dark = asset_path(FileIconIdentity::file(name), Appearance::Dark);
            assert_ne!(light, dark, "a theme swap reloads the image");
            let light = Assets.load(&light).expect("loads").expect("present");
            let dark = Assets.load(&dark).expect("loads").expect("present");
            assert_ne!(light, dark, "{name} brightens on dark");
        }
    }

    #[test]
    fn names_resolve_case_insensitively_past_parent_dots() {
        assert_eq!(
            path(FileIconIdentity::file("C:\\project\\PACKAGE.JSON")),
            "file-icons/files/node.svg",
            "exact"
        );
        assert_eq!(
            path(FileIconIdentity::file("folder.css/unknown")),
            "file-icons/files/document.svg",
            "parent dot"
        );
        assert_eq!(
            path(FileIconIdentity::file("config/.env.local")),
            "file-icons/files/gear.svg",
            "dotfile"
        );
    }

    #[test]
    fn the_longest_compound_extension_wins() {
        assert_eq!(
            path(FileIconIdentity::file("component.test.tsx")),
            "file-icons/files/react-test.svg",
            "test"
        );
        assert_eq!(
            path(FileIconIdentity::file("model.schema.json")),
            "file-icons/files/brackets-yellow.svg",
            "schema"
        );
    }

    #[test]
    fn folders_and_hints_resolve() {
        assert_eq!(
            path(FileIconIdentity::directory("src", false)),
            "file-icons/folders/folder-orange-code.svg",
            "src"
        );
        assert_eq!(
            path(FileIconIdentity::directory("unknown", true)),
            "file-icons/folders/folder.svg",
            "plain"
        );
        let rust = crate::syntax::language_for_alias("rust").expect("rust");
        assert_eq!(
            path(FileIconIdentity::file("untitled").with_language(rust)),
            "file-icons/files/rust.svg",
            "language"
        );
        assert_eq!(
            path(FileIconIdentity::file("download").with_mime_type("image/png; charset=binary")),
            "file-icons/files/image.svg",
            "mime"
        );
        assert!(has_specific_file_icon("src/component.test.tsx"), "specific");
        assert!(!has_specific_file_icon("version.unknown"), "generic");
    }

    #[test]
    fn every_manifest_reference_is_embedded() {
        let referenced = MANIFEST
            .file
            .iter()
            .chain(MANIFEST.folder.iter())
            .chain(MANIFEST.file_extensions.values())
            .chain(MANIFEST.file_names.values())
            .chain(MANIFEST.folder_names.values())
            .chain(MANIFEST.language_ids.values());
        for definition in referenced {
            let asset = definition_asset(definition).unwrap_or_else(|| panic!("{definition}"));
            assert!(EmbeddedFileIcons::get(asset).is_some(), "{definition} -> {asset}");
        }
        assert!(Assets.list(ASSET_PREFIX).expect("lists").len() >= 350, "the whole bundle");
    }
}
