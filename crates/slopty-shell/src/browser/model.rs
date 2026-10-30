// Derived from zeron (MIT, github.com/zeronsh/zeron @ 189115f), see THIRD_PARTY_NOTICES.md
//! Browser state shared by the chrome and platform host. No native handles.
use std::net::IpAddr;

/// `NSEvent` supplies characters, while GPUI bindings name non-printing keys.
/// Shift+Tab can arrive as `NSBackTabCharacter` rather than a literal tab.
#[cfg(any(target_os = "macos", test))]
pub(super) fn native_shortcut_key(characters: &str) -> &str {
    match characters {
        "\t" | "\u{19}" => "tab",
        other => other,
    }
}

#[cfg(test)]
mod native_shortcut_tests {
    use super::*;

    #[test]
    fn native_tabs_match_the_configured_navigation_keystrokes() {
        for (characters, modifiers, binding) in [
            ("\t", "ctrl-", "ctrl-tab"),
            ("\t", "ctrl-shift-", "ctrl-shift-tab"),
            ("\u{19}", "ctrl-shift-", "ctrl-shift-tab"),
            ("\t", "alt-", "alt-tab"),
            ("l", "cmd-", "cmd-l"),
        ] {
            let native = format!("{modifiers}{}", native_shortcut_key(characters));
            assert_eq!(
                gpui::Keystroke::parse_with_os(&native, crate::keys::OS).unwrap(),
                gpui::Keystroke::parse(binding).unwrap(),
            );
        }
    }
}

/// What a tab's page is at: its address, title, loading state, history and failure.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PageState {
    /// The address shown: the one asked for until the page commits, then the page's.
    pub url: Option<String>,
    /// The page's title; empty until it names one.
    pub title: String,
    /// Whether the page is loading.
    pub loading: bool,
    /// Whether Back has a page to go to.
    pub can_back: bool,
    /// Whether Forward has a page to go to.
    pub can_forward: bool,
    /// Why the page failed, in words for the person.
    pub error: Option<String>,
}

impl PageState {
    /// The tab's name: the page's title, else its host, else "Browser".
    pub fn label(&self) -> String {
        if self.title.trim().is_empty() {
            self.url
                .as_deref()
                .and_then(|s| url::Url::parse(s).ok())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "Browser".into())
        } else {
            self.title.clone()
        }
    }
}

/// Whether `url` names this device: `localhost`, a `.localhost` name or a loopback address.
pub fn loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    }
}

const LOCAL_FILE_ADDRESS: &str = "Local files can't be opened in the Browser. Open them from Files or from a file link in the chat.";

/// A filesystem path or `file:` URL typed as an address. Left to the web
/// parser, `/Users/me/page.html` loaded `https://users/me/page.html`,
/// `\\server\share\page.html` `https://server/share/page.html` and `~`
/// `https://~/` (#962).
fn local_file_address(text: &str) -> bool {
    let drive = matches!(
        text.as_bytes(),
        [letter, b':', b'/' | b'\\', ..] if letter.is_ascii_alphabetic()
    );
    (text.starts_with('/') && !text.starts_with("//"))
        || text.starts_with('\\')
        || text == "~"
        || text.starts_with("~/")
        || text.starts_with("./")
        || text.starts_with("../")
        || drive
        || text.get(..5).is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
}

/// What the person typed as the address a tab loads: `https://` added to a bare host, `http://`
/// to a bare loopback one. `Err` says, in words for the person, why it is refused.
pub fn normalize_address(input: &str) -> Result<String, &'static str> {
    let text = input.trim();
    if text.is_empty() {
        return Err("Enter a website or localhost address.");
    }
    if text.chars().any(char::is_control) {
        return Err("This address contains invalid characters.");
    }
    if local_file_address(text) {
        return Err(LOCAL_FILE_ADDRESS);
    }
    // A bare host:port looks like a URI scheme to a URL parser. Only accept
    // that ambiguity when the suffix is an actual numeric port.
    let authority = text.split(['/', '?', '#']).next().unwrap_or(text);
    let host_port = authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
    let explicit =
        text.contains("://") || (text.contains(':') && !host_port && !text.starts_with('['));
    let mut parsed =
        url::Url::parse(&if explicit { text.to_owned() } else { format!("https://{text}") })
            .ok()
            .ok_or("Enter a valid website or localhost address.")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Only http and https addresses are supported.");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Use an address without an embedded username or password.");
    }
    if !explicit && loopback(&parsed) {
        // From https to http, two special schemes: the change cannot be refused.
        let _refused = parsed.set_scheme("http");
    }
    Ok(parsed.into())
}

/// Chat links require an explicit web authority; never infer a scheme or
/// silently strip controls, credentials, malformed escapes or backslashes.
pub fn transcript_address(input: &str) -> Result<String, &'static str> {
    let lower = input.to_ascii_lowercase();
    let authority = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or("Only explicit http and https links are supported.")?;
    if input.chars().any(|c| c.is_control() || c.is_whitespace())
        || input.contains('\\')
        || authority.is_empty()
        || authority.starts_with(['/', '?', '#'])
    {
        return Err("This link contains an invalid address.");
    }
    for (i, byte) in input.bytes().enumerate() {
        if byte == b'%'
            && !input
                .as_bytes()
                .get(i.saturating_add(1)..i.saturating_add(3))
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
        {
            return Err("This link contains an invalid escape.");
        }
    }
    normalize_address(input)
}

/// Whether a page may go to `address`: an http or https address with a host and no
/// credentials.
pub fn allowed_navigation(address: &str) -> bool {
    url::Url::parse(address).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

/// Subframe navigations: web content, including a frame's own documents
/// (`about:blank`, `about:srcdoc`, `data:`, `blob:`) — never a scheme that
/// leaves the page (`file:`, `javascript:`, app links).
pub fn allowed_frame_navigation(address: &str) -> bool {
    allowed_navigation(address)
        || url::Url::parse(address).is_ok_and(|u| matches!(u.scheme(), "about" | "data" | "blob"))
}

/// How a tab's native page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presentation {
    /// Not shown: its pane is not laid out.
    #[default]
    Hidden,
    /// Shown and taking input.
    Live,
    /// Keep rendering while GPUI owns drag input.
    Passthrough,
}

/// How a page shows when its pane is laid out (`active`) and while the panes are dragged.
pub const fn presentation(active: bool, dragging: bool) -> Presentation {
    if !active {
        Presentation::Hidden
    } else if dragging {
        Presentation::Passthrough
    } else {
        Presentation::Live
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_web_addresses_and_loopback_ports() {
        for (input, expected) in [
            ("localhost:3000", "http://localhost:3000/"),
            ("127.0.0.1:5173/a?x=1", "http://127.0.0.1:5173/a?x=1"),
            ("[::1]:8080", "http://[::1]:8080/"),
            ("[::1]", "http://[::1]/"),
            (" app.localhost:3000 ", "http://app.localhost:3000/"),
            ("example.com/path", "https://example.com/path"),
            ("https://localhost:3000", "https://localhost:3000/"),
            ("http://example.com", "http://example.com/"),
        ] {
            assert_eq!(normalize_address(input), Ok(expected.into()), "{input}");
        }
    }
    #[test]
    fn rejects_non_web_schemes_credentials_and_bad_input() {
        for input in [
            "",
            "javascript:alert(1)",
            "file:///tmp/a",
            "data:text/html,hi",
            "slopty://open/chat/a",
            "https://user:pass@example.com",
            "https://",
            "two words",
            "https://example.com/\nsecret",
        ] {
            assert!(normalize_address(input).is_err(), "{input}");
        }
        assert!(!allowed_navigation("javascript:alert(1)"));
        assert!(!allowed_navigation("https://user@example.com/"));
    }
    #[test]
    fn local_paths_say_where_files_open_instead_of_loading_a_host() {
        for input in [
            "/Users/me/Documents/Codex/mockup.html",
            " /tmp/report.html ",
            "~",
            "~/Documents/mockup.html",
            "./mockup.html",
            "../mockup.html",
            "file:///Users/me/mockup.html",
            "FILE:/Users/me/mockup.html",
            r"C:\Users\me\mockup.html",
            "c:/Users/me/mockup.html",
            r"\\server\share\mockup.html",
        ] {
            assert_eq!(normalize_address(input), Err(LOCAL_FILE_ADDRESS), "{input}");
        }
    }
    #[test]
    fn frames_load_web_content_but_never_leave_the_page() {
        for ok in [
            "https://example.com/embed",
            "about:blank",
            "about:srcdoc",
            "data:text/html,hi",
            "blob:https://example.com/0f3e",
        ] {
            assert!(allowed_frame_navigation(ok), "{ok}");
        }
        for blocked in [
            "slopty://open/chat/a",
            "ms-settings:privacy",
            "file:///C:/Windows/win.ini",
            "javascript:alert(1)",
            "mailto:a@example.com",
            "",
        ] {
            assert!(!allowed_frame_navigation(blocked), "{blocked}");
        }
    }
    #[test]
    fn native_visibility_never_outlives_its_surface() {
        assert_eq!(presentation(false, false), Presentation::Hidden);
        assert_eq!(presentation(false, true), Presentation::Hidden);
        assert_eq!(presentation(true, true), Presentation::Passthrough);
        assert_eq!(presentation(true, false), Presentation::Live);
    }
    #[test]
    fn tab_labels_fall_back_to_host_then_browser() {
        let mut page = PageState::default();
        assert_eq!(page.label(), "Browser");
        page.url = Some("http://localhost:3000/path".into());
        assert_eq!(page.label(), "localhost");
        page.title = "Local preview".into();
        assert_eq!(page.label(), "Local preview");
    }
}
