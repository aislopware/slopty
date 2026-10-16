//! What an agent shows beyond words, read the same way whichever agent showed it.
//!
//! An MCP result's blocks (its text, its pictures, the resources it links), its structured
//! content, and a call of Slopty's own `show_visual` or `show_page` as the visual or page it
//! shows.
//!
//! The decoders of Codex, ACP agents and pi use these on their own records; Claude Code's
//! decoder reads its own blocks ([`crate::conversation`]).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use slopty_proto::thread::detail::Clip;
use slopty_proto::thread::{Clipped, Image, ToolDetail};
use slopty_proto::visual::{
    PageDetail, PageSource, ResourceLink, SHOW_PAGE, SHOW_VISUAL, VisualDetail,
};

use crate::mcp::MCP_SERVER_NAME;

/// The most of a visual's script or a page's HTML a call carries: what `show_visual` and
/// `show_page` take at most, so neither is ever cut.
pub const SHOWN_MAX: usize = 512 * 1024;

/// A structured result kept whole up to this; past it, its head.
const STRUCTURED: Clip = Clip { lines: 2_000, chars: 64 * 1024 };

/// An MCP App's call result, for its page, kept whole up to this (pictures in it included).
pub const RESULT: Clip = Clip { lines: usize::MAX, chars: 4 * 1024 * 1024 };

/// An MCP result's blocks, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpOutput {
    /// Its text blocks, one after another; an embedded text resource's text too.
    pub text: String,
    /// Its pictures, kept ([`crate::media`]).
    pub images: Vec<Image>,
    /// The resources it links or embeds, in its order.
    pub links: Vec<ResourceLink>,
}

/// The blocks of an MCP result (`content`), as MCP spells them: `text`, `image`, `audio`,
/// `resource_link` and `resource`.
#[must_use]
pub fn mcp_content(blocks: &[Value]) -> McpOutput {
    let mut out = McpOutput::default();
    let mut text = Vec::new();
    for block in blocks {
        let field = |name: &str| block.get(name).and_then(Value::as_str);
        match field("type") {
            Some("text") => text.extend(field("text").map(str::to_owned)),
            Some("image") => {
                if let Some(image) = field("data")
                    .and_then(|data| crate::media::keep_base64(data, field("mimeType")))
                {
                    out.images.push(image);
                }
            }
            Some("resource_link") => {
                if let Some(link) = link_of(block) {
                    out.links.push(link);
                }
            }
            Some("resource") => {
                let Some(resource) = block.get("resource") else { continue };
                let mime = resource.get("mimeType").and_then(Value::as_str);
                let blob = resource.get("blob").and_then(Value::as_str);
                let picture = blob.filter(|_| mime.is_some_and(|m| m.starts_with("image/")));
                if let Some(image) = picture.and_then(|b| crate::media::keep_base64(b, mime)) {
                    out.images.push(image);
                    continue;
                }
                if let Some(said) = resource.get("text").and_then(Value::as_str) {
                    text.push(said.to_owned());
                }
                if let Some(link) = link_of(resource) {
                    out.links.push(link);
                }
            }
            // Audio and what MCP adds later: kept as the JSON they came as, so nothing is lost.
            Some(_) | None => text.push(block.to_string()),
        }
    }
    out.text = text.join("\n");
    out
}

fn link_of(block: &Value) -> Option<ResourceLink> {
    let field = |name: &str| block.get(name).and_then(Value::as_str).map(str::to_owned);
    let uri = field("uri")?;
    Some(ResourceLink {
        name: field("name").unwrap_or_else(|| uri.clone()),
        uri,
        title: field("title"),
        mime_type: field("mimeType"),
    })
}

/// A result's `structuredContent`, as JSON.
#[must_use]
pub fn structured(value: Option<&Value>) -> Option<Clipped> {
    value.filter(|v| !v.is_null()).map(|v| Clipped::head(&v.to_string(), STRUCTURED, None))
}

/// Which of Slopty's own showing tools `tool` of MCP server `server` is: [`SHOW_VISUAL`] or
/// [`SHOW_PAGE`] of Slopty's server.
#[must_use]
pub fn slopty_tool(server: &str, tool: &str) -> Option<&'static str> {
    (server == MCP_SERVER_NAME).then(|| showing(tool)).flatten()
}

/// Which of Slopty's showing tools a call is, by the name its agent gives it.
///
/// Claude Code's `mcp__slopty__show_visual`, Codex's `slopty.show_visual`, or an ACP agent's
/// title, which names the tool and its server in its own words (`show_visual (slopty MCP
/// Server)`).
#[must_use]
pub fn named(name: &str) -> Option<&'static str> {
    let name = name.trim();
    let tool = name
        .strip_prefix("mcp__slopty__")
        .or_else(|| name.strip_prefix("slopty."))
        .or_else(|| name.strip_prefix("slopty__"))
        .or_else(|| name.strip_prefix("slopty/"))
        .or_else(|| {
            let (tool, said) = name.split_once(' ')?;
            said.contains(MCP_SERVER_NAME).then_some(tool)
        })?;
    showing(tool)
}

fn showing(tool: &str) -> Option<&'static str> {
    [SHOW_VISUAL, SHOW_PAGE].into_iter().find(|t| *t == tool)
}

/// The visual or page a call of `tool` ([`SHOW_VISUAL`] or [`SHOW_PAGE`]) with `input` shows,
/// its paths made absolute against `cwd`: `None` for input that does not say what to show.
#[must_use]
pub fn shown(tool: &str, input: &Value, cwd: &str) -> Option<ToolDetail> {
    let text = |name: &str| input.get(name).and_then(Value::as_str);
    let title = text("title").unwrap_or_default().to_owned();
    match tool {
        SHOW_VISUAL => {
            let script = text("script")?;
            let assets: BTreeMap<String, String> = input
                .get("assets")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, path)| Some((name.clone(), absolute(path.as_str()?, cwd))))
                .collect();
            Some(ToolDetail::Visual(VisualDetail { title, script: whole(script), assets }))
        }
        SHOW_PAGE => {
            let source = match (text("html"), text("file")) {
                (Some(html), _) => PageSource::Html(whole(html)),
                (None, Some(file)) => PageSource::File(absolute(file, cwd)),
                (None, None) => return None,
            };
            Some(ToolDetail::Page(PageDetail { title, source }))
        }
        _ => None,
    }
}

fn whole(text: &str) -> Clipped {
    Clipped::head(text, Clip { lines: usize::MAX, chars: SHOWN_MAX }, None)
}

/// `path` against `cwd` when it is relative, `~/` from the home.
fn absolute(path: &str, cwd: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest).to_string_lossy().into_owned();
    }
    if Path::new(path).is_absolute() || cwd.is_empty() {
        return path.to_owned();
    }
    Path::new(cwd).join(path).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// An MCP result's text, pictures and links are each read for what they are; a picture's
    /// digest is that of its bytes.
    #[test]
    fn an_mcp_results_blocks_are_read_for_what_they_are() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x04\x00\x00\x00\x02\x08\x06\x00\x00\x07";
        let data = data_encoding::BASE64.encode(png);
        let out = mcp_content(&[
            json!({ "type": "text", "text": "two rows" }),
            json!({ "type": "image", "data": data, "mimeType": "image/png" }),
            json!({ "type": "resource_link", "uri": "file:///r/a.csv", "name": "a.csv",
                    "mimeType": "text/csv" }),
            json!({ "type": "resource", "resource": { "uri": "mem://note", "text": "a note" } }),
            json!({ "type": "resource", "resource": { "uri": "mem://pic", "mimeType": "image/png",
                    "blob": data } }),
        ]);
        assert_eq!(out.text, "two rows\na note");
        assert_eq!(out.images.len(), 2);
        assert_eq!(out.images[0].digest, blake3::hash(png).to_hex().to_string());
        assert_eq!((out.images[0].width, out.images[0].height), (4, 2));
        let uris: Vec<&str> = out.links.iter().map(|l| l.uri.as_str()).collect();
        assert_eq!(uris, ["file:///r/a.csv", "mem://note"]);
        assert_eq!(out.links[0].mime_type.as_deref(), Some("text/csv"));
        assert_eq!(structured(Some(&json!({ "rows": 2 }))).unwrap().text, r#"{"rows":2}"#);
        assert_eq!(structured(Some(&Value::Null)), None);
    }

    /// Slopty's showing tools are known by every agent's name for them, and nothing else is.
    #[test]
    fn slopty_s_showing_tools_are_known_by_every_agents_name() {
        assert_eq!(named("mcp__slopty__show_visual"), Some(SHOW_VISUAL));
        assert_eq!(named("slopty.show_page"), Some(SHOW_PAGE));
        assert_eq!(named("show_page (slopty MCP Server)"), Some(SHOW_PAGE));
        assert_eq!(named("mcp__other__show_visual"), None);
        assert_eq!(named("show_visual"), None, "another server's tool of the same name");
        assert_eq!(slopty_tool("slopty", "show_visual"), Some(SHOW_VISUAL));
        assert_eq!(slopty_tool("slopty", "agent_start"), None);
        assert_eq!(slopty_tool("charts", "show_visual"), None);
    }

    /// A call's script or HTML is kept byte for byte, and its paths are the worker's own.
    #[test]
    fn a_call_shows_its_visual_or_page_byte_for_byte() {
        let script = "local s = ui.state {}\nreturn function() return ui.text \"hi\" end";
        let input = json!({ "script": script, "title": "Hi", "assets": { "a.png": "img/a.png" } });
        let Some(ToolDetail::Visual(visual)) = shown(SHOW_VISUAL, &input, "/work") else {
            panic!()
        };
        assert_eq!((visual.title.as_str(), visual.script.text.as_str()), ("Hi", script));
        assert!(!visual.script.is_clipped());
        assert_eq!(visual.assets["a.png"], "/work/img/a.png");
        let page = json!({ "file": "out/chart.html", "title": "Chart" });
        let Some(ToolDetail::Page(page)) = shown(SHOW_PAGE, &page, "/work") else { panic!() };
        assert_eq!(page.source, PageSource::File("/work/out/chart.html".to_owned()));
        let html = json!({ "html": "<svg/>", "title": "Dot" });
        let Some(ToolDetail::Page(page)) = shown(SHOW_PAGE, &html, "/work") else { panic!() };
        assert_eq!(page.source, PageSource::Html(Clipped::whole("<svg/>")));
        assert_eq!(shown(SHOW_PAGE, &json!({ "title": "x" }), "/work"), None);
        assert_eq!(shown(SHOW_VISUAL, &json!({ "title": "x" }), "/work"), None);
    }
}
