//! What a page may reach, written down: its origin, its Content Security Policy, its content
//! rule list, and the trusted host document that holds the view in a sandboxed frame.
//!
//! Each page is served from an origin of its own, `slopty-page://<id>`, by a scheme handler that
//! answers only that host. The host document comes with a policy that lets the view run its own
//! inline scripts and styles and load what its origin serves, and nothing else: no network, no
//! forms, no base; frames are the navigation delegate's to refuse. The view's frame inherits the
//! policy. A rule list blocks every
//! `http(s)` and `ws(s)` load besides, so a policy widened by a grant still reaches only the
//! origins granted (`docs/decisions/platform.md`, "Pages run in a sandbox of their own").

use std::fmt::Write as _;
use std::hash::{Hash as _, Hasher as _};

use super::{Network, NetworkError, PageId, Refusal, Theme};

/// The scheme every page is served under; a page's id is its host.
pub(crate) const SCHEME: &str = "slopty-page";

/// Where the host document's policy sends its violation reports: the scheme handler counts them
/// (a navigation the view tried is one).
pub(crate) const REPORTS: &str = "/.slopty/csp";

/// The CSS variable whose value is the page's background, which a native shows opaque.
pub(crate) const BACKGROUND: &str = "--color-background-primary";

/// The background when the theme names none.
const FALLBACK_BACKGROUND: (&str, &str) = ("#ffffff", "#000000");

/// The origin page `page` is served from.
pub(crate) fn origin(page: PageId) -> String {
    format!("{SCHEME}://{}", page.host())
}

/// An origin a page may be granted: `scheme://host[:port]`, the host possibly `*.`-prefixed for
/// its subdomains, as MCP Apps' `connectDomains` and `resourceDomains` name them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
    /// `*.host`: any subdomain of `host`, not `host` itself.
    subdomains: bool,
}

impl Origin {
    /// Read `text` as an origin; refused when it is not `http`, `https`, `ws` or `wss`, has a
    /// path, a user, or a character a host name has not.
    pub(crate) fn parse(text: &str) -> Result<Self, NetworkError> {
        let refused = || NetworkError::Origin(text.to_owned());
        let (scheme, rest) = text.split_once("://").ok_or_else(refused)?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https" | "ws" | "wss") {
            return Err(refused());
        }
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => (host, Some(port.parse::<u16>().ok().ok_or_else(refused)?)),
            None => (rest, None),
        };
        let (subdomains, host) = match host.strip_prefix("*.") {
            Some(host) => (true, host),
            None => (false, host),
        };
        let host = host.to_ascii_lowercase();
        let fits = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.';
        let labels_fit = host.split('.').all(|label| !label.is_empty());
        if host.is_empty() || !host.chars().all(fits) || !labels_fit {
            return Err(refused());
        }
        Ok(Self { scheme, host, port, subdomains })
    }

    /// A rule list's URL filter matching exactly its URLs. `WebKit`'s filters are a subset of
    /// regular expressions without alternation.
    fn url_filter(&self) -> String {
        let host = self.host.replace('.', r"\.");
        let star = if self.subdomains { r"([a-z0-9-]+\.)+" } else { "" };
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("^{}://{star}{host}{port}[/?#]", self.scheme)
    }
}

/// How a policy and MCP Apps' `connectDomains` name it.
impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let star = if self.subdomains { "*." } else { "" };
        write!(f, "{}://{star}{}", self.scheme, self.host)?;
        match self.port {
            Some(port) => write!(f, ":{port}"),
            None => Ok(()),
        }
    }
}

/// The origins a grant opens, read and checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Grants {
    connect: Vec<Origin>,
    resources: Vec<Origin>,
}

impl Grants {
    pub(crate) fn read(network: &Network) -> Result<Self, NetworkError> {
        let read = |list: &[String]| -> Result<Vec<Origin>, NetworkError> {
            list.iter().map(String::as_str).map(Origin::parse).collect()
        };
        Ok(Self { connect: read(&network.connect)?, resources: read(&network.resources)? })
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.connect.is_empty() && self.resources.is_empty()
    }

    /// What may be fetched and opened sockets to.
    pub(crate) fn connect(&self) -> &[Origin] {
        &self.connect
    }

    /// What scripts, styles, images, fonts and media may come from.
    pub(crate) fn resources(&self) -> &[Origin] {
        &self.resources
    }

    /// How many origins it opens.
    pub(crate) const fn reach(&self) -> usize {
        self.connect.len().saturating_add(self.resources.len())
    }
}

/// The policy the host document is served with, which the view's frame inherits.
///
/// Frames are left to the navigation delegate (`frame-src *`), which refuses every one but the
/// view's own `about:srcdoc`: a navigation the policy blocked would leave the view's frame blank,
/// where one the delegate cancels leaves the view as it was.
pub(crate) fn policy(page: PageId, grants: &Grants) -> String {
    let own = origin(page);
    let list = |origins: &[Origin]| -> String {
        origins.iter().fold(String::new(), |mut all, o| {
            let _wrote = write!(all, " {o}");
            all
        })
    };
    let connect = list(&grants.connect);
    let resources = list(&grants.resources);
    format!(
        "default-src 'none'; \
         script-src {own} 'unsafe-inline'{resources}; \
         style-src {own} 'unsafe-inline'{resources}; \
         img-src {own} data: blob:{resources}; \
         font-src {own} data: blob:{resources}; \
         media-src {own} data: blob:{resources}; \
         connect-src {own}{connect}; \
         frame-src *; object-src 'none'; worker-src 'none'; manifest-src 'none'; \
         form-action 'none'; base-uri 'none'; report-uri {REPORTS}"
    )
}

/// The content rule list: every `http(s)` and `ws(s)` load blocked, but those to the origins
/// granted.
pub(crate) fn rules(grants: &Grants) -> String {
    let block = |filter: &str| serde_json::json!({ "trigger": { "url-filter": filter }, "action": { "type": "block" } });
    let mut list = vec![block("^https?://"), block("^wss?://")];
    for origin in grants.connect.iter().chain(&grants.resources) {
        list.push(serde_json::json!({
            "trigger": { "url-filter": origin.url_filter() },
            "action": { "type": "ignore-previous-rules" },
        }));
    }
    serde_json::Value::Array(list).to_string()
}

/// The name a compiled rule list is kept under: one per distinct list.
pub(crate) fn rules_identifier(rules: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rules.hash(&mut hasher);
    format!("slopty-page-{:016x}", hasher.finish())
}

/// What a report of the host document's policy says was refused: a navigation of the view's
/// frame (`frame-src`), or another load.
pub(crate) fn report(body: &[u8]) -> Option<Refusal> {
    let report: serde_json::Value = serde_json::from_slice(body).ok()?;
    let report = report.get("csp-report").or_else(|| report.get("body"))?;
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| report.get(*key).and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .to_owned()
    };
    let directive = text(&["effective-directive", "effectiveDirective", "violated-directive"]);
    let url = text(&["blocked-uri", "blockedURL"]);
    if directive.is_empty() {
        return None;
    }
    Some(if directive == "frame-src" || directive == "child-src" {
        Refusal::Navigation { url }
    } else {
        Refusal::Blocked { directive, url }
    })
}

/// The path of a request for one of the page's files, without its leading `/`, when it names a
/// file under the page's folder: no `.` or `..` part, no empty part.
pub(crate) fn relative(path: &str) -> Option<&str> {
    let relative = path.strip_prefix('/')?;
    let fits =
        !relative.is_empty() && relative.split('/').all(|part| !matches!(part, "" | "." | ".."));
    fits.then_some(relative)
}

/// The variables of `theme` a page may be given: a name `--` and letters, digits, `-` or `_`,
/// and a value with nothing that could end a declaration, a rule or an element.
pub(crate) fn variables(theme: &Theme) -> impl Iterator<Item = (&str, &str)> {
    theme.variables.iter().map(|(n, v)| (n.as_str(), v.as_str())).filter(|(name, value)| {
        let named = name.strip_prefix("--").is_some_and(|rest| {
            !rest.is_empty()
                && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
        let clean = value.len() <= 512
            && !value
                .chars()
                .any(|c| matches!(c, ';' | '{' | '}' | '<' | '>' | '"' | '\\') || c.is_control());
        named && clean
    })
}

/// The colour behind the view: the theme's background variable, else white or black.
pub(crate) fn background(theme: &Theme) -> &str {
    variables(theme).find(|(name, _)| *name == BACKGROUND).map_or(
        if theme.dark { FALLBACK_BACKGROUND.1 } else { FALLBACK_BACKGROUND.0 },
        |(_, value)| value,
    )
}

const fn scheme_of(theme: &Theme) -> &'static str {
    if theme.dark { "dark" } else { "light" }
}

/// The view's own document with the theme's variables set on its root, after its doctype so it
/// stays in standards mode.
pub(crate) fn themed(view: &str, theme: &Theme) -> String {
    let mut style = format!("<style id=\"slopty-theme\">:root{{color-scheme:{}", scheme_of(theme));
    for (name, value) in variables(theme) {
        let _wrote = write!(style, ";{name}:{value}");
    }
    style.push_str("}</style>");
    let trimmed = view.trim_start();
    let doctype = trimmed.get(..9).is_some_and(|head| head.eq_ignore_ascii_case("<!doctype"));
    match trimmed.find('>').filter(|_| doctype) {
        Some(end) => {
            let (head, rest) = trimmed.split_at(end.saturating_add(1));
            format!("{head}{style}{rest}")
        }
        None => format!("{style}{view}"),
    }
}

/// `text` safe inside a double-quoted attribute or as an element's text.
pub(crate) fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// The trusted document a page is served as: no script of its own, and the view in a frame
/// sandboxed to scripts alone (an opaque origin: no storage, no cookies, no pop-ups, no
/// top-level navigation, no forms, no downloads, no permissions). The frame fills it, so it
/// has nothing to scroll; its root keeps its overflow, since an `overflow: hidden` root turns
/// the web view's own scroll view off on iOS, and a scroll view that cannot scroll between the
/// frame's and the window's stops UIKit handing a finger at the frame's edge on to the chat
/// (gpui-fast's `docs/composition.md`, "Scrolling over a native").
pub(crate) fn host_document(title: &str, view: &str, theme: &Theme) -> String {
    let title = escaped(title);
    let background = background(theme);
    let scheme = scheme_of(theme);
    let view = escaped(&themed(view, theme));
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <meta name=\"color-scheme\" content=\"{scheme}\">\
         <style>:root{{color-scheme:{scheme}}}html,body{{margin:0;padding:0;height:100%;\
         background:{background}}}iframe{{display:block;border:0;width:100%;\
         height:100%;background:{background}}}</style></head><body>\
         <iframe id=\"view\" title=\"{title}\" sandbox=\"allow-scripts\" srcdoc=\"{view}\">\
         </iframe></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme {
            dark: true,
            variables: vec![
                (BACKGROUND.to_owned(), "#101418".to_owned()),
                ("--font-sans".to_owned(), "Inter, sans-serif".to_owned()),
                ("--bad;".to_owned(), "red".to_owned()),
                ("--evil".to_owned(), "red}</style><script>".to_owned()),
            ],
        }
    }

    /// The policy opens the page's own origin and nothing else until a grant names more.
    #[test]
    fn the_policy_reaches_only_the_pages_own_origin() {
        let policy = policy(PageId(0x2a), &Grants::default());
        assert!(policy.contains("connect-src slopty-page://2a;"), "{policy}");
        assert!(policy.contains("frame-src *;"), "the delegate refuses frames, keeping the view");
        assert!(policy.contains("default-src 'none'"));
        assert!(!policy.contains("http"), "{policy}");
        let grants = Grants::read(&Network {
            connect: vec!["https://api.example.com".to_owned()],
            resources: vec!["https://*.cdn.example".to_owned()],
        })
        .unwrap();
        let policy = super::policy(PageId(0x2a), &grants);
        assert!(policy.contains("connect-src slopty-page://2a https://api.example.com;"));
        assert!(policy.contains("img-src slopty-page://2a data: blob: https://*.cdn.example;"));
    }

    /// The rule list blocks the web and web sockets, one rule each, and lets each granted origin
    /// through exactly.
    #[test]
    fn the_rules_block_the_network_but_what_is_granted() {
        let closed: serde_json::Value = serde_json::from_str(&rules(&Grants::default())).unwrap();
        assert_eq!(closed.as_array().map(Vec::len), Some(2));
        let grants = Grants::read(&Network {
            connect: vec!["http://127.0.0.1:5173".to_owned(), "wss://*.example.com".to_owned()],
            resources: Vec::new(),
        })
        .unwrap();
        let open = rules(&grants);
        assert!(open.contains(r"^http://127\\.0\\.0\\.1:5173[/?#]"), "{open}");
        assert!(open.contains(r"^wss://([a-z0-9-]+\\.)+example\\.com[/?#]"), "{open}");
        assert_ne!(rules_identifier(&open), rules_identifier(&rules(&Grants::default())));
    }

    /// Only an origin of the web or its sockets, without a path, a user or a stray character, is
    /// granted.
    #[test]
    fn an_origin_is_checked_before_it_is_granted() {
        for good in
            ["https://a.example", "http://localhost:8080", "wss://*.b.example", "HTTPS://A.B/"]
        {
            assert!(Origin::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "a.example",
            "file:///etc",
            "javascript://x",
            "https://a.example/path",
            "https://user@a.example",
            "https://a..example",
            "https://a.example:99999",
            "https://",
            "https://a b",
        ] {
            assert!(Origin::parse(bad).is_err(), "{bad}");
        }
    }

    /// The host document escapes the view into its frame, sandboxed to scripts alone, with the
    /// theme's background behind it and only its clean variables on the view's root.
    #[test]
    fn the_host_document_frames_the_view_with_the_theme() {
        let doc = host_document("A \"chart\"", "<!DOCTYPE html><p>x & y</p>", &theme());
        assert!(doc.contains("sandbox=\"allow-scripts\""));
        assert!(doc.contains("title=\"A &quot;chart&quot;\""));
        assert!(doc.contains("background:#101418"));
        assert!(
            doc.contains("srcdoc=\"&lt;!DOCTYPE html&gt;&lt;style id=&quot;slopty-theme&quot;&gt;")
        );
        assert!(doc.contains("--color-background-primary:#101418;--font-sans:Inter, sans-serif}"));
        assert!(doc.contains("x &amp; y"));
        assert!(!doc.contains("--bad"), "a bad name is dropped");
        assert!(!doc.contains("--evil"), "a value that ends the style is dropped");
        assert_eq!(doc.matches("<iframe").count(), 1);
        assert!(!doc.contains("overflow"), "the root's scroll view stays on");
    }

    /// A report of a navigation the view tried is a refused navigation; any other a blocked load.
    #[test]
    fn a_policy_report_names_what_was_refused() {
        let navigation = br#"{"csp-report":{"document-uri":"slopty-page://1/","effective-directive":"frame-src","blocked-uri":"https://example.com"}}"#;
        assert_eq!(
            report(navigation),
            Some(Refusal::Navigation { url: "https://example.com".to_owned() })
        );
        let image = br#"{"csp-report":{"effective-directive":"img-src","blocked-uri":"https://a.example/x.png"}}"#;
        assert_eq!(
            report(image),
            Some(Refusal::Blocked {
                directive: "img-src".to_owned(),
                url: "https://a.example/x.png".to_owned()
            })
        );
        assert_eq!(report(b"{}"), None);
        assert_eq!(report(b"nonsense"), None);
    }

    /// A file is asked for by its path under the page's folder, never above it.
    #[test]
    fn a_files_path_stays_under_the_pages_folder() {
        assert_eq!(relative("/data.csv"), Some("data.csv"));
        assert_eq!(relative("/models/a.glb"), Some("models/a.glb"));
        for refused in ["/", "/../x", "/a/../../x", "/a//b", "/./a", "x", "/a/"] {
            assert_eq!(relative(refused), None, "{refused}");
        }
    }

    /// A view without a doctype gets the theme first; one with a doctype keeps it first.
    #[test]
    fn the_theme_goes_after_a_doctype() {
        let light = Theme { dark: false, variables: Vec::new() };
        assert!(
            themed("<p>hi</p>", &light)
                .starts_with("<style id=\"slopty-theme\">:root{color-scheme:light}")
        );
        assert!(themed("  <!doctype html><p>", &light).starts_with("<!doctype html><style"));
        assert_eq!(background(&light), "#ffffff");
    }
}
