// Derived from zeron (MIT, github.com/zeronsh/zeron @ 77a3c0f), see THIRD_PARTY_NOTICES.md
//! What the composer's pickers left for the rest of the shell: the in-app folder browser's
//! navigation (the add-space flow), the agents' brand marks and which agents show, and the
//! reasoning levels' names.
//!
//! TUI-first (`docs/decisions/agents.md`), the composer's pickers (project, branch,
//! agent and model, traits) are gone: an agent's TUI picks its own model and reasoning.
#![allow(
    missing_docs,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::too_long_first_doc_paragraph,
    reason = "shell lint migration: slice A"
)]

use slopty_view_proto::{FolderListing, HarnessId, ReasoningLevel};

use crate::catalog::registry::HarnessDescriptor;

// ---------------------------------------------------------------------------
// Pure: labels
// ---------------------------------------------------------------------------

pub const fn reasoning_label(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal => "Minimal",
        ReasoningLevel::Low => "Low",
        ReasoningLevel::Medium => "Medium",
        ReasoningLevel::High => "High",
        ReasoningLevel::XHigh => "X-High",
        ReasoningLevel::Max => "Max",
        ReasoningLevel::Ultra => "Ultra",
        ReasoningLevel::Ultracode => "Ultracode",
        ReasoningLevel::Ultrathink => "Ultrathink",
    }
}

// ---------------------------------------------------------------------------
// Pure: folder-browser navigation (used by the shell's add-space flow)
// ---------------------------------------------------------------------------

/// Windows browser paths are parsed by shape even on a POSIX client: the
/// device being browsed can be remote. A UNC root includes BOTH server and
/// share; extended paths keep their prefix and use only backslash separators.
struct WindowsBrowserPath<'a> {
    root: &'a str,
    rest: &'a str,
    extended: bool,
}

impl WindowsBrowserPath<'_> {
    const fn is_separator(&self, ch: char) -> bool {
        ch == '\\' || (!self.extended && ch == '/')
    }

    fn root_path(&self) -> String {
        let root = if self.extended { self.root.to_owned() } else { self.root.replace('/', "\\") };
        format!("{root}\\")
    }

    fn normalized_rest(&self) -> String {
        let rest = self.rest.trim_matches(|ch| self.is_separator(ch));
        if self.extended { rest.to_owned() } else { rest.replace('/', "\\") }
    }
}

fn windows_browser_path(path: &str) -> Option<WindowsBrowserPath<'_>> {
    let extended = path.starts_with(r"\\?\");
    let start = if extended { 4 } else { 0 };
    let tail = &path[start..];
    let bytes = tail.as_bytes();
    if bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes.get(2).is_none_or(|b| *b == b'\\' || (!extended && *b == b'/'))
    {
        return Some(WindowsBrowserPath {
            root: &path[..start + 2],
            rest: &path[start + 2..],
            extended,
        });
    }
    let unc_start = if extended {
        tail.get(..4).filter(|prefix| prefix.eq_ignore_ascii_case("UNC\\"))?;
        8
    } else if path.starts_with(r"\\") && !path.starts_with(r"\\.\") {
        2
    } else {
        return None;
    };
    let is_separator = |ch| ch == '\\' || (!extended && ch == '/');
    let server_end = unc_start + path[unc_start..].find(is_separator)?;
    if server_end == unc_start {
        return None;
    }
    let share_start = server_end + 1;
    let share_end =
        path[share_start..].find(is_separator).map_or(path.len(), |at| share_start + at);
    if share_end == share_start {
        return None;
    }
    Some(WindowsBrowserPath { root: &path[..share_end], rest: &path[share_end..], extended })
}

/// Drive- or UNC-rooted: separators are backslashes.
pub fn is_windows_path(path: &str) -> bool {
    windows_browser_path(path).is_some()
}

/// Lexical identity for browser rows, crumbs and existing spaces. Preserve
/// POSIX spelling and directory casing (Windows directories can be case
/// sensitive). Prefix aliases are equivalent only for ordinary Windows names.
pub fn folder_path_key(path: &str) -> String {
    let Some(windows) = windows_browser_path(path) else {
        return path.to_owned();
    };
    let rest = windows.normalized_rest();
    let ordinary_name = |name: &str| {
        let stem = name.split('.').next().unwrap_or_default();
        let reserved = matches!(stem.to_ascii_uppercase().as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem.get(..3).is_some_and(|prefix| {
                (prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT"))
                    && matches!(
                        &stem[3..],
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
            });
        !name.ends_with(['.', ' '])
            && !name.contains(['/', ':', '*', '?', '"', '<', '>', '|'])
            && !name.chars().any(char::is_control)
            && !reserved
    };
    let unc_root = windows.extended
        && windows.root.get(4..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC\\"));
    let ordinary_names = rest.split('\\').all(ordinary_name)
        && (!unc_root || windows.root[8..].split('\\').all(ordinary_name));
    let root = if windows.extended && ordinary_names {
        let tail = &windows.root[4..];
        if tail.get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC\\")) {
            format!(r"\\{}", &tail[4..])
        } else {
            tail.to_owned()
        }
    } else if windows.extended {
        windows.root.to_owned()
    } else {
        windows.root.replace('/', "\\")
    };
    // Drive letters never distinguish case (`c:` is `C:`). Folder names keep
    // theirs: a directory can be made case-sensitive (`fsutil
    // setCaseSensitiveInfo`), so `Repo` and `repo` may be different folders.
    let root = uppercase_drive_letter(root);
    if rest.is_empty() { format!("{root}\\") } else { format!("{root}\\{rest}") }
}

/// `c:` → `C:`, also behind an extended `\\?\` prefix; other roots unchanged.
fn uppercase_drive_letter(mut root: String) -> String {
    let drive = root.strip_prefix(r"\\?\").unwrap_or(&root);
    let bytes = drive.as_bytes();
    if bytes.len() == 2 && bytes[1] == b':' && bytes[0].is_ascii_lowercase() {
        let at = root.len() - 2;
        root[at..=at].make_ascii_uppercase();
    }
    root
}

pub fn same_folder_path(left: &str, right: &str) -> bool {
    folder_path_key(left) == folder_path_key(right)
}

/// Parent of an absolute path; `None` at the filesystem root.
pub fn parent_path(path: &str) -> Option<String> {
    if let Some(windows) = windows_browser_path(path) {
        let rest = windows.normalized_rest();
        if rest.is_empty() {
            return None;
        }
        let parent = rest.rfind('\\').map_or("", |at| &rest[..at]);
        return Some(format!("{}{parent}", windows.root_path()));
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None; // was "/" (or empty)
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_owned()),
        Some(at) => Some(trimmed[..at].to_string()),
        None => None,
    }
}

/// Join a listing path and an entry name.
pub fn child_path(base: &str, name: &str) -> String {
    if base.ends_with(['/', '\\']) {
        format!("{base}{name}")
    } else if windows_browser_path(base).is_some() {
        format!("{base}\\{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Byte length of `name`'s prefix matching `query`, compared char-for-char
/// case-insensitively; `None` when `query` isn't a prefix of `name`. The
/// length indexes into `name` (not `query`) so the completion suffix keeps
/// the folder's real casing: `("Documents", "doc") → Some(3)` → `"uments"`.
pub fn completion_prefix_len(name: &str, query: &str) -> Option<usize> {
    let mut len = 0;
    let mut name_chars = name.chars();
    for qc in query.chars() {
        let nc = name_chars.next()?;
        if !nc.to_lowercase().eq(qc.to_lowercase()) {
            return None;
        }
        len += nc.len_utf8();
    }
    Some(len)
}

/// Resolve a typed path segment against folder `names` (slash-descend):
/// exact match first — case-SENSITIVE before case-insensitive, so `GitHub/`
/// picks a `GitHub` sibling over `github` — then a unique case-insensitive
/// prefix. Ambiguity resolves to `None`: the slash stays in the query.
pub fn segment_target(names: &[&str], query: &str) -> Option<usize> {
    if let Some(ix) = names.iter().position(|n| *n == query) {
        return Some(ix);
    }
    if let Some(ix) = names.iter().position(|n| completion_prefix_len(n, query) == Some(n.len())) {
        return Some(ix);
    }
    let mut hits =
        names.iter().enumerate().filter(|(_, n)| completion_prefix_len(n, query).is_some());
    let (ix, _) = hits.next()?;
    hits.next().is_none().then_some(ix)
}

/// Whether a palette query is path-shaped (absolute, home-relative or
/// drive-rooted) rather than a folder name to filter by.
pub fn is_typed_path(query: &str) -> bool {
    query.starts_with(['/', '~']) || is_windows_path(query)
}

/// Interpret a palette query as a typed path jump: absolute (`/disk2/projects`),
/// drive-rooted (`D:\projects`) or home-relative (`~`, `~/github`). Returns the
/// absolute path to browse, trailing separator trimmed. `home` is the device's
/// resolved home — `None` until the first listing lands, when `~` can't expand
/// yet. A query like `~foo` is a folder name, not a path.
pub fn typed_path_target(query: &str, home: Option<&str>) -> Option<String> {
    let query = query.trim();
    if let Some(windows) = windows_browser_path(query) {
        let rest = windows.normalized_rest();
        return Some(if rest.is_empty() {
            windows.root_path()
        } else {
            format!("{}{rest}", windows.root_path())
        });
    }
    if let Some(rest) = query.strip_prefix('~') {
        if is_windows_path(home?) {
            let home = typed_path_target(home?, None)?;
            if rest.is_empty() {
                return Some(home);
            }
            let rest = rest.strip_prefix(['/', '\\'])?.trim_end_matches(['/', '\\']);
            return Some(if rest.is_empty() {
                home
            } else {
                child_path(&home, &rest.replace('/', "\\"))
            });
        }
        let home = home?.trim_end_matches('/');
        if rest.is_empty() {
            return Some(home.to_owned());
        }
        let rest = rest.strip_prefix('/')?.trim_end_matches('/');
        return Some(if rest.is_empty() { home.to_owned() } else { format!("{home}/{rest}") });
    }
    if query.starts_with('/') {
        let trimmed = query.trim_end_matches('/');
        return Some(if trimmed.is_empty() { "/".to_owned() } else { trimmed.to_owned() });
    }
    None
}

/// A path as typed or pasted into the new-project path field: surrounding
/// whitespace and one pair of matching quotes dropped, and shell escapes
/// (`My\ Folder`, from a folder dragged into a terminal) undone — except on
/// a `windows` device, whose backslashes are separators.
pub fn clean_typed_path(text: &str, windows: bool) -> String {
    let text = text.trim();
    let text = ['"', '\'']
        .iter()
        .find_map(|quote| text.strip_prefix(*quote)?.strip_suffix(*quote))
        .unwrap_or(text);
    unescape_typed_path(text, windows)
}

/// Shell escapes undone (`My\ Folder` → `My Folder`, `\\` → `\`), except
/// on a `windows` device, whose backslashes are separators.
pub fn unescape_typed_path(text: &str, windows: bool) -> String {
    if windows || is_windows_path(text) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push(chars.next().unwrap_or('\\')),
            ch => out.push(ch),
        }
    }
    out
}

/// A path or folder name as the path field writes it: on POSIX a literal
/// `\` is doubled, so [`unescape_typed_path`] gives it back.
pub fn escape_typed_path(text: &str, windows: bool) -> String {
    if windows || is_windows_path(text) { text.to_owned() } else { text.replace('\\', r"\\") }
}

/// `~` or `~/…`: the device's home, or a path under it.
pub fn is_home_relative(text: &str) -> bool {
    text == "~" || text.starts_with("~/") || text.starts_with(r"~\")
}

/// Lexically resolve `.`, `..` and repeated separators in an absolute
/// browser path. The device lists whatever spelling it is sent, so a typed
/// `~/code/../notes` would otherwise open (and add) a project with `..` in
/// its path. `..` stops at the root; extended (`\\?\`) paths are verbatim by
/// definition and stay as they are.
pub fn normalize_browser_path(path: &str) -> String {
    fn segments<'a>(rest: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
        let mut kept = Vec::new();
        for segment in rest {
            match segment {
                "" | "." => {}
                ".." => {
                    kept.pop();
                }
                segment => kept.push(segment),
            }
        }
        kept
    }
    if let Some(windows) = windows_browser_path(path) {
        if windows.extended {
            return path.to_owned();
        }
        let rest = segments(windows.rest.split(|ch| windows.is_separator(ch)));
        return format!("{}{}", windows.root_path(), rest.join("\\"));
    }
    format!("/{}", segments(path.split('/')).join("/"))
}

/// Where a path typed in the new-project path field points, absolute and
/// normalized: `~` and `~/x` under the device's `home`, absolute and
/// drive-rooted paths as typed, anything else relative to `base` (the open
/// folder) — `../site`, `src`. On a `windows` device (or one whose paths
/// say so) `/` is `\`, `\code` is on the open folder's drive and `D:code`
/// at that drive's root. `None` when that takes a `home` or `base` not
/// known yet.
pub fn resolve_typed_path(
    text: &str,
    base: Option<&str>,
    home: Option<&str>,
    windows: bool,
) -> Option<String> {
    let windows = windows || base.or(home).is_some_and(is_windows_path);
    let mut text = clean_typed_path(text, windows);
    if windows {
        // `//nas/share` is a share, `/code` a drive's root.
        text = text.replace('/', r"\");
    }
    if text.is_empty() {
        return None;
    }
    let drive_relative = |text: &str| {
        let bytes = text.as_bytes();
        bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
    };
    let target = match typed_path_target(&text, home) {
        Some(target) => target,
        None if is_home_relative(&text) => return None,
        None if windows && text.starts_with('\\') => {
            let root = windows_browser_path(base?)?.root_path();
            child_path(&root, text.trim_start_matches('\\'))
        }
        None if windows && drive_relative(&text) => format!(r"{}\{}", &text[..2], &text[2..]),
        None => child_path(base?, &text),
    };
    Some(normalize_browser_path(&target))
}

/// A typed path split at its last separator: the folder part as typed
/// (separator included, empty for a bare name) and the name being typed
/// after it. `\` separates only on a `windows` device or in a
/// Windows-shaped path.
pub fn split_typed_leaf(text: &str, windows: bool) -> (&str, &str) {
    let windows = windows || is_windows_path(text);
    let at = if windows { text.rfind(['/', '\\']) } else { text.rfind('/') };
    match at {
        Some(at) => (&text[..=at], &text[at + 1..]),
        None => ("", text),
    }
}

/// Breadcrumb segments for a path: `(label, full path)`, root first.
pub fn breadcrumbs(path: &str) -> Vec<(String, String)> {
    if let Some(windows) = windows_browser_path(path) {
        let root = windows.root_path();
        let mut out = vec![(root.clone(), root.clone())];
        let mut acc = root;
        for segment in windows.rest.split(|ch| windows.is_separator(ch)).filter(|s| !s.is_empty()) {
            acc = child_path(&acc, segment);
            out.push((segment.to_owned(), acc.clone()));
        }
        return out;
    }
    let root = "/".to_owned();
    let mut out = vec![(root.clone(), root)];
    let mut acc = String::new();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        acc.push('/');
        acc.push_str(segment);
        out.push((segment.to_owned(), acc.clone()));
    }
    out
}

/// Directory rows of a listing (files never render in the browser).
pub fn browser_rows(listing: &FolderListing) -> Vec<&slopty_view_proto::FolderEntry> {
    listing.entries.iter().filter(|e| e.is_dir).collect()
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

/// Brand mark + optional tint for a harness (the mock harness scripts Claude-flavoured runs, so
/// it wears the Claude mark).
pub(crate) fn harness_brand_icon(harness: HarnessId) -> (&'static str, Option<gpui::Hsla>) {
    match harness {
        HarnessId::CLAUDE_CODE | HarnessId::MOCK => {
            (crate::icons::CLAUDE_MARK, Some(crate::icons::claude_brand()))
        }
        HarnessId::CODEX => (crate::icons::OPENAI_MARK, None),
        HarnessId::CURSOR => (crate::icons::CURSOR_MARK, None),
        // Cognition's mark (the Devin product icon), monochrome.
        HarnessId::DEVIN => (crate::icons::DEVIN_MARK, None),
        // Monochrome mark, tinted by the surface like OpenAI's.
        HarnessId::GROK => (crate::icons::GROK_MARK, None),
        // Nous Research's mark (the Hermes product icon), monochrome.
        HarnessId::HERMES => (crate::icons::HERMES_MARK, None),
        HarnessId::PI => (crate::icons::PI_MARK, None),
        // The pixel-"o" from opencode's wordmark (their favicon), monochrome.
        HarnessId::OPENCODE => (crate::icons::OPENCODE_MARK, None),
        HarnessId::ANTIGRAVITY => (crate::icons::ANTIGRAVITY_MARK, None),
        // Any other agent (an ACP agent by its registry's name) wears the agent mark.
        _ => (crate::icons::BOT, None),
    }
}

/// `SLOPTY_HARNESS=mock` (the e2e/dev rig) opts the mock harness into the UI;
/// production launches never set it, so the mock never surfaces there.
fn mock_harness_enabled() -> bool {
    std::env::var("SLOPTY_HARNESS").ok().as_deref().map(str::trim) == Some("mock")
}

/// Production pickers AND chip resolution hide the mock harness — the
/// registry always lists it, but it must never surface in real UI (neither in
/// the picker rail nor as the eager default the chips resolve against).
/// `SLOPTY_HARNESS=mock` shows it; otherwise it only remains when it's
/// literally all there is (a dev build with no real harness registered).
pub fn visible_harnesses(list: &[HarnessDescriptor]) -> Vec<HarnessDescriptor> {
    visible_harnesses_impl(list, mock_harness_enabled())
}

fn visible_harnesses_impl(list: &[HarnessDescriptor], allow_mock: bool) -> Vec<HarnessDescriptor> {
    if allow_mock {
        return list.to_vec();
    }
    let real: Vec<HarnessDescriptor> =
        list.iter().filter(|d| d.id != HarnessId::MOCK).cloned().collect();
    if real.is_empty() { list.to_vec() } else { real }
}

#[cfg(test)]
mod tests {
    use slopty_view_proto::FolderEntry;

    use super::*;

    #[test]
    fn folder_paths_and_breadcrumbs() {
        assert_eq!(parent_path("/home/w/dev"), Some("/home/w".to_owned()));
        assert_eq!(parent_path("/home"), Some("/".to_owned()));
        assert_eq!(parent_path("/home/"), Some("/".to_owned()));
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path(""), None);
        assert_eq!(child_path("/home", "w"), "/home/w");
        assert_eq!(child_path("/", "home"), "/home");
        let crumbs = breadcrumbs("/home/w/dev");
        let labels: Vec<&str> = crumbs.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["/", "home", "w", "dev"]);
        assert_eq!(crumbs[2].1, "/home/w");
        assert_eq!(breadcrumbs("/").len(), 1);
    }

    #[test]
    fn windows_folder_paths_and_breadcrumbs() {
        assert_eq!(parent_path(r"D:\Random\slopty"), Some(r"D:\Random".to_owned()));
        assert_eq!(parent_path(r"D:\Random"), Some(r"D:\".to_owned()));
        assert_eq!(parent_path(r"D:\Random\"), Some(r"D:\".to_owned()));
        assert_eq!(parent_path(r"D:\"), None);
        assert_eq!(parent_path("D:"), None);
        assert_eq!(child_path(r"D:\", "Random"), r"D:\Random");
        assert_eq!(child_path(r"D:\Random", "slopty"), r"D:\Random\slopty");
        let crumbs = breadcrumbs(r"D:\Random\slopty");
        let labels: Vec<&str> = crumbs.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, [r"D:\", "Random", "slopty"]);
        assert_eq!(crumbs[0].1, r"D:\");
        assert_eq!(crumbs[1].1, r"D:\Random");
        assert_eq!(breadcrumbs(r"D:\").len(), 1);
        assert!(!is_windows_path("/D:/x"));
        assert!(!is_windows_path("ab:/x"));
    }

    #[test]
    fn extended_and_unc_folder_navigation_preserves_roots() {
        for root in [r"\\?\D:\", r"\\server\share\", r"\\?\UNC\server\share\"] {
            let folder = format!("{root}projects\\comet");
            let parent = format!("{root}projects");
            assert_eq!(parent_path(&folder), Some(parent.clone()));
            assert_eq!(parent_path(&parent), Some(root.to_owned()));
            assert_eq!(parent_path(root), None);
            assert_eq!(child_path(&parent, "comet"), folder);
            let crumbs = breadcrumbs(&folder);
            assert_eq!(
                crumbs,
                vec![
                    (root.to_owned(), root.to_owned()),
                    ("projects".into(), parent),
                    ("comet".into(), folder.clone()),
                ]
            );
            assert!(is_typed_path(&folder));
            assert_eq!(typed_path_target(&format!("{folder}\\"), None), Some(folder));
            assert_eq!(typed_path_target(root, None), Some(root.to_owned()));
        }
        // The server alone isn't a browsable share, and device namespaces
        // must not be mistaken for a server/share pair.
        assert!(!is_windows_path(r"\\server"));
        assert!(!is_windows_path(r"\\server\\repo"));
        assert!(!is_windows_path(r"\\.\C:\repo"));
        assert!(!is_windows_path(r"\\?\Volume{123}\repo"));
    }

    #[test]
    fn windows_folder_identity_handles_prefixes_without_changing_posix_names() {
        assert!(same_folder_path(r"C:\Users\Ana\repo", r"\\?\C:\Users\Ana\repo"));
        assert!(same_folder_path("C:/Users/Ana/repo/", r"\\?\C:\Users\Ana\repo"));
        assert!(same_folder_path(r"\\server\share\repo", r"\\?\UNC\server\share\repo"));
        assert!(!same_folder_path(r"\\server\share\repo", r"\\server\other\repo"));
        assert!(!same_folder_path(r"C:\repo", r"D:\repo"));
        assert!(!same_folder_path(r"C:\Repo", r"C:\repo"));
        // Only the drive letter is case-insensitive.
        assert!(same_folder_path(r"c:\Users\Ana", r"C:\Users\Ana"));
        assert!(same_folder_path("c:/Users/Ana/", r"\\?\C:\Users\Ana"));
        assert!(same_folder_path(r"\\?\c:\repo.", r"\\?\C:\repo."));
        assert!(same_folder_path(r"d:\", r"D:\"));
        assert!(!same_folder_path(r"c:\users\ana", r"C:\Users\Ana"));
        // Extended paths can refer to names with different Win32 semantics.
        for name in ["repo.", "repo ", "..", "NUL.txt", "COM¹"] {
            assert!(!same_folder_path(&format!(r"C:\{name}"), &format!(r"\\?\C:\{name}")));
        }
        for path in
            ["/home/ana/repo", "/Users/Ana/repo", r"/tmp/name\with\backslashes", "//server/share"]
        {
            assert_eq!(folder_path_key(path), path);
        }
        assert!(!same_folder_path("/home/Ana", "/home/ana"));
    }

    #[test]
    fn home_relative_jumps_use_the_remote_windows_separator() {
        for home in [
            r"C:\Users\Ana",
            r"\\?\C:\Users\Ana",
            r"\\server\share\Ana",
            r"\\?\UNC\server\share\Ana",
        ] {
            assert_eq!(
                typed_path_target("~/code/comet/", Some(home)),
                Some(format!("{home}\\code\\comet"))
            );
            assert_eq!(
                typed_path_target(r"~\code\comet\", Some(home)),
                Some(format!("{home}\\code\\comet"))
            );
        }
        assert_eq!(typed_path_target("~/code", Some("/home/ana")), Some("/home/ana/code".into()));
        assert_eq!(typed_path_target("~/code", Some("/Users/Ana")), Some("/Users/Ana/code".into()));
        assert_eq!(typed_path_target(r"~\code", Some("/home/ana")), None);
    }

    #[test]
    fn completion_prefix_lengths() {
        // Case-insensitive; the length indexes into the NAME's bytes.
        assert_eq!(completion_prefix_len("Documents", "doc"), Some(3));
        assert_eq!(&"Documents"[3..], "uments");
        assert_eq!(completion_prefix_len("slopty", "slopty"), Some(6)); // the renamed name is one longer.
        assert_eq!(completion_prefix_len("slopty", ""), Some(0));
        assert_eq!(completion_prefix_len("slopty", "dev"), None);
        // Longer than the name → not a prefix.
        assert_eq!(completion_prefix_len("dev", "devel"), None);
        // Multibyte names slice on a char boundary.
        assert_eq!(completion_prefix_len("héllo", "hé"), Some(3));
        assert_eq!(&"héllo"[3..], "llo");
    }

    #[test]
    fn segment_target_resolution() {
        let names = ["github", "GitHub", "worktree"];
        // Exact casing beats the earlier case-insensitive sibling…
        assert_eq!(segment_target(&names, "GitHub"), Some(1));
        assert_eq!(segment_target(&names, "github"), Some(0));
        // …but with no exact-cased hit, case-insensitive exact still lands.
        assert_eq!(segment_target(&names, "WORKTREE"), Some(2));
        // Unique prefix descends; an ambiguous one keeps the slash honest.
        assert_eq!(segment_target(&names, "work"), Some(2));
        assert_eq!(segment_target(&names, "g"), None);
        assert_eq!(segment_target(&names, "x"), None);
    }

    #[test]
    fn typed_path_target_expands_absolute_and_home_paths() {
        let home = Some("/home/wing");
        assert_eq!(typed_path_target("/disk2/", home), Some("/disk2".into()));
        assert_eq!(typed_path_target("/disk2/projects", home), Some("/disk2/projects".into()));
        assert_eq!(typed_path_target("/", home), Some("/".into()));
        assert_eq!(typed_path_target("~", home), Some("/home/wing".into()));
        assert_eq!(typed_path_target("~/", home), Some("/home/wing".into()));
        assert_eq!(typed_path_target("~/github/", home), Some("/home/wing/github".into()));
        // `~x` is a folder name; relative queries are searches, not paths.
        assert_eq!(typed_path_target("~x", home), None);
        assert_eq!(typed_path_target("src", home), None);
        // `~` can't expand before the device's home is known.
        assert_eq!(typed_path_target("~/github", None), None);
        assert_eq!(typed_path_target("/disk2", None), Some("/disk2".into()));
    }

    #[test]
    fn typed_path_target_accepts_windows_drive_paths() {
        let home = Some(r"C:\Users\wing");
        assert_eq!(typed_path_target(r"D:\", home), Some(r"D:\".into()));
        assert_eq!(typed_path_target("D:", home), Some(r"D:\".into()));
        assert_eq!(typed_path_target("D:/", home), Some(r"D:\".into()));
        assert_eq!(typed_path_target(r"D:\Random\slopty\", home), Some(r"D:\Random\slopty".into()));
        // Forward slashes normalise so the crumb trail can match the path.
        assert_eq!(typed_path_target("D:/Random/slopty", None), Some(r"D:\Random\slopty".into()));
        assert!(is_typed_path(r"D:\x"));
        assert!(is_typed_path("/x") && is_typed_path("~"));
        assert!(!is_typed_path("src") && !is_typed_path("ab:/x"));
    }

    #[test]
    fn pasted_paths_lose_quotes_and_shell_escapes() {
        let clean = |text| clean_typed_path(text, false);
        assert_eq!(clean("  /Users/ana/code \n"), "/Users/ana/code");
        assert_eq!(clean("'/Users/ana/My Code'"), "/Users/ana/My Code");
        assert_eq!(clean("\"~/My Code\""), "~/My Code");
        // A folder dragged into a terminal.
        assert_eq!(clean(r"/Users/ana/My\ Code\ \(old\)"), "/Users/ana/My Code (old)");
        // Mismatched quotes are part of the name.
        assert_eq!(clean("'/tmp/x\""), "'/tmp/x\"");
        // Windows backslashes are separators, drive-rooted or relative.
        assert_eq!(clean(r"D:\Random\slopty"), r"D:\Random\slopty");
        assert_eq!(clean_typed_path(r"'..\site'", true), r"..\site");
        assert_eq!(clean_typed_path(r"~\code", true), r"~\code");
    }

    #[test]
    fn typed_paths_resolve_dots_home_and_the_open_folder() {
        let (base, home) = (Some("/home/wing/code"), Some("/home/wing"));
        let resolve = |text| resolve_typed_path(text, base, home, false);
        assert_eq!(resolve("/srv//data/"), Some("/srv/data".into()));
        assert_eq!(resolve("~/code/../notes"), Some("/home/wing/notes".into()));
        assert_eq!(resolve("~"), Some("/home/wing".into()));
        // Relative to the open folder, like a shell's cd.
        assert_eq!(resolve("slopty/src"), Some("/home/wing/code/slopty/src".into()));
        assert_eq!(resolve("../site"), Some("/home/wing/site".into()));
        assert_eq!(resolve("./"), Some("/home/wing/code".into()));
        // `..` stops at the root.
        assert_eq!(resolve("/../../etc"), Some("/etc".into()));
        assert_eq!(resolve("/"), Some("/".into()));
        assert_eq!(resolve("  "), None);
        // `~` needs the device's home, a relative path an open folder.
        assert_eq!(resolve_typed_path("~/code", base, None, false), None);
        assert_eq!(resolve_typed_path("code", None, home, false), None);
        assert_eq!(resolve_typed_path("/srv/data", None, None, false), Some("/srv/data".into()));
        // A backslash in a POSIX name, as the field writes it, survives.
        let written = escape_typed_path(r"/home/wing/a\b/", false);
        assert_eq!(written, r"/home/wing/a\\b/");
        assert_eq!(resolve(written.as_str()), Some(r"/home/wing/a\b".into()));
    }

    #[test]
    fn typed_windows_paths_resolve_on_any_client() {
        let (base, home) = (Some(r"C:\Users\Ana\code"), Some(r"C:\Users\Ana"));
        let resolve = |text| resolve_typed_path(text, base, home, false);
        // Rooted on the open folder's drive, or at a named drive's root.
        assert_eq!(resolve(r"\Projects"), Some(r"C:\Projects".into()));
        assert_eq!(resolve("/Projects"), Some(r"C:\Projects".into()));
        assert_eq!(resolve("D:work"), Some(r"D:\work".into()));
        // `/` reads as `\`, so a forward-slash share stays a share.
        assert_eq!(resolve("//nas/projects/app"), Some(r"\\nas\projects\app".into()));
        assert_eq!(escape_typed_path(r"C:\a", false), r"C:\a");
        // A Windows device before anything listed: rooted paths wait for it.
        assert_eq!(resolve_typed_path("/code", None, None, true), None);
        assert_eq!(resolve(r"D:\Random\..\slopty\"), Some(r"D:\slopty".into()));
        assert_eq!(resolve("D:/Random/./slopty"), Some(r"D:\Random\slopty".into()));
        assert_eq!(resolve(r"~\notes"), Some(r"C:\Users\Ana\notes".into()));
        assert_eq!(resolve(r"..\site"), Some(r"C:\Users\Ana\site".into()));
        assert_eq!(resolve(r"C:\.."), Some(r"C:\".into()));
        assert_eq!(resolve(r"\\server\share\team\..\repo"), Some(r"\\server\share\repo".into()));
        // Verbatim paths are taken as they are.
        assert_eq!(normalize_browser_path(r"\\?\D:\a\..\b"), r"\\?\D:\a\..\b");
    }

    #[test]
    fn typed_paths_split_at_the_name_being_typed() {
        let split = |text| split_typed_leaf(text, false);
        assert_eq!(split("/home/wing/co"), ("/home/wing/", "co"));
        assert_eq!(split("~/"), ("~/", ""));
        assert_eq!(split("code"), ("", "code"));
        assert_eq!(split(r"D:\Random\ze"), (r"D:\Random\", "ze"));
        assert_eq!(split("D:/Random/ze"), ("D:/Random/", "ze"));
        // A backslash is part of a POSIX name, a separator on Windows.
        assert_eq!(split(r"/tmp/a\b"), ("/tmp/", r"a\b"));
        assert_eq!(split_typed_leaf(r"code\ze", true), (r"code\", "ze"));
        assert_eq!(split_typed_leaf(r"~\co", true), (r"~\", "co"));
    }

    #[test]
    fn browser_navigation_reducer() {
        let listing = FolderListing {
            path: "/home/w".into(),
            entries: vec![
                FolderEntry { name: "notes.txt".into(), is_dir: false, is_repo: false },
                FolderEntry { name: "dev".into(), is_dir: true, is_repo: false },
                FolderEntry { name: "slopty".into(), is_dir: true, is_repo: true },
            ],
            truncated: false,
        };
        // Files never show as rows.
        assert_eq!(browser_rows(&listing).len(), 2);
        assert_eq!(browser_rows(&listing)[1].name, "slopty");
    }

    #[test]
    fn mock_harness_hidden_unless_alone() {
        let descriptor = |id: HarnessId, name: &str| HarnessDescriptor {
            id,
            name: name.into(),
            supports_steering: true,
            steering_mode: slopty_view_proto::SteeringMode::StepBoundary,
            reasoning_levels: vec![],
            installed: true,
            can_install: false,
            enabled: None,
        };
        let mixed = vec![
            descriptor(HarnessId::MOCK, "Mock"),
            descriptor(HarnessId::CLAUDE_CODE, "Claude Code"),
        ];
        // Env-independent core: mock hidden in production…
        let visible = visible_harnesses_impl(&mixed, false);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, HarnessId::CLAUDE_CODE);
        let only_mock = vec![descriptor(HarnessId::MOCK, "Mock")];
        assert_eq!(visible_harnesses_impl(&only_mock, false).len(), 1);
        // …and opted back in by SLOPTY_HARNESS=mock (the e2e rig).
        assert_eq!(visible_harnesses_impl(&mixed, true).len(), 2);
        assert_eq!(visible_harnesses_impl(&mixed, true)[0].id, HarnessId::MOCK);
    }
}
