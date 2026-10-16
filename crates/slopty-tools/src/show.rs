//! What an agent shows the person inline, beyond words.
//!
//! A native visual (`show_visual`, a Luau script Slopty runs in its sandbox and draws with its
//! own elements) and a page (`show_page`, HTML or an HTML or SVG file, shown in a sandboxed web
//! view with no network).
//!
//! Neither needs the server: the call stays in the agent's own record with its whole input, and
//! the worker's decoder shows it from there (`slopty_agent::shown`). What the tool does is
//! check it first, so a script that fails, or a page that would load from the network it does
//! not have, comes back to the agent with what to fix (a script's error with its line), and the
//! person never sees it broken.
//!
//! A script is run only where Slopty's sandbox is built in (the `visual` feature, which
//! `slopty mcp` on a worker has); the server's own build, which never runs one, answers
//! `show_visual` with where it does.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ToolError;

/// The largest page `show_page` takes, as HTML or as a file: what a thread keeps whole.
pub const PAGE_MAX: usize = 512 * 1024;

/// The largest PDF or table file `show_page` takes: what the worker reads of a thread's file
/// (`slopty_worker::thread::assets::MAX_ASSET_BYTES`).
const DOC_MAX: u64 = 8 * 1024 * 1024;
/// The largest script `show_visual` takes.
pub const SCRIPT_MAX: usize = 256 * 1024;
/// The most assets one visual ships with.
pub const ASSETS_MAX: usize = 16;
/// The largest asset.
pub const ASSET_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// All of a visual's assets together.
pub const ASSETS_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// `show_visual`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShowVisualArgs {
    /// The Luau visual script: setup at the top level, ending with `return function() return
    /// ui.column { ... } end`.
    script: String,
    /// A short name for the visual.
    title: String,
    /// Files the script uses, by the name it uses them: `{ "prism.glb": "models/prism.glb" }`.
    /// Paths are on this machine, relative to your working directory. glTF models (.glb, or
    /// .gltf with everything embedded), PNG, JPEG, WebP and Radiance .hdr images; at most 16
    /// files, 8 MB each, 10 MB together.
    #[serde(default)]
    assets: BTreeMap<String, String>,
}

/// `show_page`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShowPageArgs {
    /// The page: an HTML document or fragment, or an SVG that starts with `<svg`. Give this or
    /// `file`.
    html: Option<String>,
    /// An `.html`, `.htm` or `.svg` file on this machine, relative to your working directory:
    /// shown as it is, its relative files (`./data.csv`) beside it, and shown again when it
    /// changes. A `.pdf`, `.csv` or `.tsv` file shows natively: a PDF's pages, a table. Give
    /// this or `html`.
    file: Option<String>,
    /// A short name for the page.
    title: String,
}

pub(crate) const SHOW_VISUAL_DESCRIPTION: &str = "\
Show an interactive visual (chart, table, dashboard, diagram, simulation, 3D view) inline in this \
chat, above your final text reply; call it before writing that reply. Use it when a visual says more \
than prose. The reader already sees it, so the reply should not announce, locate, or restate it: add \
only what the visual doesn't say.\n\
\n\
A visual is a Luau script (Lua 5.1 plus `+=`, `continue`, and `if` expressions). Slopty runs it in a \
sandbox (no I/O or network; math.random is seeded, so data is the same on every render) and renders \
what it returns with Slopty's own native interface. Build data and state at the top level, then return \
a view function. Slopty calls the view again after every handler, so handlers just assign to state:\n\
\n\
local s = ui.state { metric = \"p95\" }\n\
local data = { { region = \"us-east\", p95 = 182, p99 = 240 }, { region = \"eu-west\", p95 = 140, p99 = 210 } }\n\
return function()\n\
  return ui.column { gap = 16,\n\
    ui.text { \"Latency by region\", size = \"lg\", weight = 600 },\n\
    ui.tabs { options = { \"p95\", \"p99\" }, value = s.metric, on_change = function(v) s.metric = v end },\n\
    ui.chart { type = \"bar\", height = 220, unit = \"ms\",\n\
      x = ui.map(data, function(d) return d.region end),\n\
      series = { { name = s.metric, values = ui.map(data, function(d) return d[s.metric] end) } } },\n\
  }\n\
end\n\
\n\
Slopty runs the script before showing it (setup, render, every draw, and each tab, toggle, slider, and \
button once) and returns the first error with its line; fix it and call again.\n\
\n\
Nodes are props tables: children are the array part, strings are plain text, nil and false children \
are skipped, and lists of nodes flatten (so ui.map results can be children). Unknown props are errors.\n\
- ui.column { gap = 8, align = \"start|center|end\", ... }, ui.row { gap = 12, align, justify = \
\"start|center|end|between\", wrap = true, ... }, ui.grid { columns = 3, gap = 16, ... }. Any node \
takes width = px or grow = true inside a row (sliders, charts, tables, canvases, and scenes fill a \
row's spare width by default), and key = \"name\" to keep its hover or camera when the tree changes \
shape. A row or column with on_click = function() end is clickable; hover = true gives \
it the hover wash.\n\
- ui.text { \"plain \", ui.span { \"bold\", weight = 600, color = \"accent\", mono = true }, size = \
\"xs|sm|body|lg|xl|2xl\", weight = 400-700, color, mono, italic, align, truncate }, or ui.text \"string\".\n\
- ui.stat { value = \"182 ms\", label = \"p95 latency\", delta = \"-12%\", tone = \"success\" }: a headline \
figure over a muted label; tone colors the delta.\n\
- ui.table { columns = { \"Region\", { label = \"p95\", align = \"end\" } }, rows = { { \"us-east\", 182 }, \
... } }: laid out like a markdown table (bold header, hairline row rules), numbers right-aligned; cells \
are strings, numbers, or nodes.\n\
- ui.chart { type = \"bar|line|area|scatter\", height = 220, x = { labels }, series = { { name, values = \
{ ... }, color } }, stacked, horizontal (bar), min, max, unit = \"ms\", prefix = \"$\", decimals, legend }. \
Scatter series take points = { { x, y }, ... } (x_unit, x_decimals). Axes, gridlines, the legend, and a \
hover readout are built in.\n\
- ui.rule {} is a hairline divider; ui.space { size = 16 }; ui.link { label, url = \"https://...\" } \
opens like a chat link.\n\
- Controls: ui.tabs { options = { ... }, value, on_change = function(option) end }, ui.toggle { label, \
value = bool, on_change = function(on) end }, ui.slider { label, value, min, max, step, unit, on_change \
= function(n) end }, ui.button { label, on_click = function() end, primary = bool }.\n\
- ui.canvas { height = 240, draw = function(g, w, h, t) ... end, animate = bool, on_click = \
function(x, y) end }: 2D drawing in pixels, origin top-left, w = the reply column's width. \
g:line(x1, y1, x2, y2, { color, width, dash }), g:polyline(pts, { color, width }), g:polygon(pts, \
{ fill, stroke, width }), g:rect(x, y, w, h, { fill, stroke, radius }), g:circle(x, y, r, { fill, \
stroke }), g:arc(x, y, r, a0, a1, { color } or { fill, inner = radius } for pie and donut sectors; \
radians clockwise from 3 o'clock), g:text(x, y, str, { size, color, weight, align = \
\"start|center|end\", baseline = \"top|middle|bottom\", mono }). pts are { x1, y1, x2, y2, ... } or \
{ { x, y }, ... }. g.pointer is { x, y, down } while the pointer is over the canvas (draw hover \
effects with it); t is seconds, and animate = true redraws every frame while visible.\n\
- ui.scene { height = 320, draw = function(s, t) ... end, orbit = true, spin = 0.3 (turntable \
radians per second), camera = { yaw = 0.6, pitch = 0.45, zoom = 1 }, animate = bool }: real 3D, y up, \
rendered on the GPU with depth, soft shadows from a key light, and bloom. Slopty frames the camera on \
your geometry and the reader drags to orbit. Solids (box, sphere, cylinder, surface, mesh) take \
material props: roughness (0 mirror to 1 matte), metal (0-1), glow (emits its color and blooms; \
use for lamps, beams, highlights), glass = true (refracts what's behind it; ior = 1.5, dispersion \
= 0-1 splits it into a rainbow, as a prism does). Build real forms from several primitives (legs, \
tops, rims), not single boxes, and give materials so surfaces read as wood, metal, or glass. s:box { at = { x, y, z }, size = { w, h, d } or n, color }, s:sphere { at, radius, color }, \
s:cylinder { at (base center), radius, height, color }, s:surface { x = { min, max }, z = { min, max }, \
f = function(x, z) return y end or values = { { ... } }, detail = 32, colors = { \"chart1\", \"chart3\" } \
(a ramp by height), wire = bool }, s:mesh { vertices = { { x, y, z }, ... }, faces = { { 1, 2, 3 }, ... }, \
color }, s:line(a, b, { color, width }), s:path(points, { color, width }), s:point(at, { radius, \
color, hint = \"shown on hover\" }), s:label(at, text, { color, size }), s:grid { size = 10, step = 1 }, \
s:axes { size = 1, labels = { \"x\", \"y\", \"z\" } }. Physical colors may be \"#rrggbb\" in scenes \
(a material's own color: gold, glass, wood), and only there.\n\
- Assets: pass real files in show_visual's assets (name -> path) and scripts use them by name. \
s:model { asset = \"prism.glb\", at, rotate = { x, y, z } degrees, scale, fit = 2 (center it and size \
its longest side), color and material props override the model's own } draws a glTF model with its \
materials and textures. Solids take texture = \"wood.jpg\" (multiplied by color; mesh takes uvs = { { u, v \
}, ... } per vertex). s:environment { asset = \"studio.hdr\", intensity = 1, background = false } lights \
the scene from an equirectangular image (an HDR is best) that metal and glass reflect and glass bends; \
background = true also draws it behind the scene. s:sprite { at, size = 48 (pixels), color, glow = 1, \
asset = \"flare.png\" } adds a camera-facing light (a flare or glint; a soft round glow without an \
asset) that blooms. ui.shader takes textures = { \"rock.jpg\", ... } as iTexture0-iTexture3 \
(repeating, mipmapped). Find or make assets with your own tools (download, convert KTX2 or EXR to PNG \
or HDR) before calling.\n\
- ui.shader { height = 360, code = [[ GLSL ]], buffers = { [[ GLSL ]], ... }, inputs = { speed = \
s.speed }, blend = \"alpha|light|opaque\", bloom = 0-3, orbit = bool, spin, camera = { yaw, pitch, \
zoom }, animate = true }: GPU shaders for what geometry can't express: raymarched SDFs and \
fractals, light and optics (beams, refraction, dispersion), volumes, fluids, particles, generative \
art. Write each pass as on Shadertoy: void mainImage(out vec4 fragColor, in vec2 fragCoord), with \
iResolution (vec3: width and height in pixels, then the display's pixel density), iTime, \
iTimeDelta, iFrame, iMouse (pixels from the bottom-left; xy where the pointer is, zw where it went \
down while it's held, negative otherwise), iCamera (vec4 yaw, pitch, zoom, 0; orbit = true lets the reader drag \
it), every inputs name as a float (bind them to state so sliders and toggles drive the shader), \
iNoise (a tiling 256x256 random RGBA texture), and iChannel0-iChannel3 with iChannelResolution: \
buffers run first, in order, each into a float texture that every pass reads as iChannel<i> (an \
earlier buffer's current frame; a buffer's own previous frame), for multi-pass work like blur, \
trails, accumulation, and simulation. The shader sits on the chat, never in a box: don't paint a \
background, vignette, or floor color. By default (blend = \"light\") the output is emitted light: \
brightness is opacity and black is clear, right for beams, glows, sparks, and light through glass. \
For solid shaded objects (fractals, SDF bodies) set blend = \"alpha\" and write alpha 0 where the ray \
misses. bloom = 0.6-1.5 adds a soft glow around bright light. \
The width varies with the reader (a phone is about 350px wide, often narrower than the shader is \
tall): scale the scene by min(iResolution.x, iResolution.y), not one axis, and keep the subject \
centered with room on both sides, so it fits and stays centered at any aspect. \
GLSL errors come back with the pass and line number.\n\
- Helpers: ui.scale(d0, d1, r0, r1) returns a mapping function; ui.ticks(lo, hi, n); ui.fmt(n, { unit, \
prefix, decimals }) groups digits; ui.map(list, function(v, i) end); ui.range(a, b, step); ui.clamp; \
ui.lerp; ui.series(i) is \"chart<i>\"; ui.color(token, alpha); ui.mix(a, b, t). print(...) output comes \
back to you.\n\
\n\
Colors are theme tokens, never hex or color names (except scene materials): text; muted (labels, axes, captions); faint \
(tertiary notes); accent (the one thing to notice); on-accent (text on an accent mark); success, \
warning, danger (status only); border (hairlines, gridlines); chart1, chart2, chart3, chart4, \
chart5, chart6 (series, in order; \
chart1 is the accent). They follow the reader's theme, light or dark, on frosted glass or an opaque \
surface, so never assume a background color.\n\
\n\
Style, enforced: layout nodes have no fill, border, shadow, padding, or font props. A visual is part \
of the reply, not a widget, so lay content directly on the chat with type, spacing, and alignment: no \
cards, tiles, boxed panels, banners, or canvas backgrounds. Stats are a row of plain ui.stat figures; \
a chart is a plain heading, then the chart; separate sections with gap or one ui.rule. Text renders \
in Slopty's interface font at the reader's text size: body for most text, sm and xs for labels and \
notes, lg and xl for headings, 2xl for a headline figure. The width is the reply column (about 720px, \
as narrow as 360px), so lay out fluidly with grow, wrap, and grid, and give charts, canvases, and \
scenes heights of 160-420px.";

pub(crate) const SHOW_PAGE_DESCRIPTION: &str = "\
Show a web page inline in this chat, above your final text reply; call it before writing that \
reply. Prefer show_visual for charts, tables, dashboards, diagrams and 3D, which Slopty draws \
natively; use show_page for what needs the web: an HTML report, an SVG, a page you built, a \
library's own output. Give `html` (a document, a fragment, or SVG starting with `<svg`) or `file` \
(an .html, .htm or .svg file; its relative files load from beside it, and the page reloads when \
the file changes; a .pdf shows its pages and a .csv or .tsv a table, natively). A page may load \
Vega, Vega-Lite and vega-embed or plotly.js from .slopty/lib/ (vega.min.js, vega-lite.min.js, \
vega-embed.min.js, plotly-cartesian.min.js); a fenced vega-lite or plotly block in your reply \
draws as a chart too.\n\
\n\
The page has no network: every script, style, font, image and datum must be inline or a file \
beside `file`. Links to http(s) resources are refused here, with the ones found; inline them. \
It runs sandboxed: no cookies, storage, popups, downloads or navigation away.\n\
\n\
Style: leave the root transparent with no outer padding or margin, and take colours and type from \
the host's CSS variables so the page follows the reader's theme, light or dark: \
var(--color-text-primary), var(--color-text-secondary), var(--color-background-primary), \
var(--color-border-primary), var(--font-sans), var(--font-mono). The width is the reply column \
(about 720px, as narrow as 360px); the height follows the content, up to 720px, and scrolls \
past it.";

/// What `show_visual` answers: the visual checked, or why not with the script's line.
pub(crate) fn visual(args: ShowVisualArgs, cwd: &Path) -> Result<Value, ToolError> {
    let ShowVisualArgs { script, title, assets } = args;
    if title.trim().is_empty() {
        return Err(ToolError::invalid("a visual needs a title"));
    }
    if script.len() > SCRIPT_MAX {
        return Err(ToolError::invalid(format!(
            "the script is {} KB; a visual's script is at most {} KB",
            script.len() / 1024,
            SCRIPT_MAX / 1024
        )));
    }
    let files = read_assets(&assets, cwd)?;
    checked(&script, &title, files)
}

#[cfg(feature = "visual")]
fn checked(script: &str, title: &str, files: Vec<(String, Vec<u8>)>) -> Result<Value, ToolError> {
    let names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
    let files =
        files.into_iter().map(|(name, bytes)| (name, std::sync::Arc::from(bytes))).collect();
    let seed = slopty_visual::seed_for(title);
    let report = slopty_visual::check_with_assets(script, seed, slopty_visual::Assets::new(files))
        .map_err(|e| ToolError::invalid(said(&e.message)))?;
    Ok(json!({
        "shown": title,
        "nodes": report.nodes,
        "controls": report.interactions,
        "assets": names,
        "printed": report.logs,
    }))
}

#[cfg(not(feature = "visual"))]
fn checked(_: &str, _: &str, _: Vec<(String, Vec<u8>)>) -> Result<Value, ToolError> {
    Err(ToolError::invalid(
        "show_visual runs a script only through `slopty mcp` on the machine you work on; this \
         build has no sandbox for one",
    ))
}

/// A script's error, its place said in words: `visual:3: boom` is `line 3: boom`.
#[cfg(feature = "visual")]
fn said(message: &str) -> String {
    let at = message.strip_prefix("visual:").and_then(|rest| {
        let (line, what) = rest.split_once(':')?;
        line.parse::<u32>().ok().map(|line| format!("line {line}:{what}"))
    });
    format!(
        "the script failed at {}; fix it and call show_visual again",
        at.as_deref().unwrap_or(message)
    )
}

/// The assets `named`, read from this machine against `cwd`, checked as a visual takes them.
fn read_assets(
    named: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<(String, Vec<u8>)>, ToolError> {
    if named.len() > ASSETS_MAX {
        return Err(ToolError::invalid(format!("a visual takes at most {ASSETS_MAX} assets")));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0_u64;
    let mut read = Vec::with_capacity(named.len());
    for (name, path) in named {
        if !valid_name(name) {
            return Err(ToolError::invalid(format!(
                "asset name '{name}' must be a plain file name (letters, digits, '.', '-', '_')"
            )));
        }
        // Kept as files, and file systems often ignore case.
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(ToolError::invalid(format!(
                "asset names must differ by more than case ('{name}')"
            )));
        }
        let bytes = read_capped(&cwd.join(path), ASSET_MAX_BYTES)
            .map_err(|why| ToolError::invalid(format!("asset '{name}': {path} {why}")))?;
        total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if total > ASSETS_MAX_BYTES {
            return Err(ToolError::invalid("a visual's assets total at most 10 MB"));
        }
        read.push((name.clone(), bytes));
    }
    Ok(read)
}

/// Whether `name` can name an asset: a plain file name, as the sandbox takes it.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// The file at `path`, at most `max` bytes, however it changes while read; why not, in words.
fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| format!("can't be read: {e}"))?;
    let meta = file.metadata().map_err(|e| format!("can't be read: {e}"))?;
    if !meta.is_file() {
        return Err("is not a file".to_owned());
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("can't be read: {e}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(format!("is over {} MB", max / (1024 * 1024)));
    }
    Ok(bytes)
}

/// What `show_page` answers: the page checked, or what to fix.
pub(crate) fn page(args: ShowPageArgs, cwd: &Path) -> Result<Value, ToolError> {
    let ShowPageArgs { html, file, title } = args;
    if title.trim().is_empty() {
        return Err(ToolError::invalid("a page needs a title"));
    }
    let max = u64::try_from(PAGE_MAX).unwrap_or(u64::MAX);
    let (text, from) = match (html, file) {
        (Some(html), None) => {
            if html.len() > PAGE_MAX {
                return Err(ToolError::invalid(format!(
                    "the page is {} KB; a page is at most {} KB",
                    html.len() / 1024,
                    PAGE_MAX / 1024
                )));
            }
            (html, None)
        }
        (None, Some(file)) => {
            let path = cwd.join(&file);
            let kind = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            // A PDF or a table the app draws itself, as large as a thread's file may be.
            if matches!(kind.as_deref(), Some("pdf" | "csv" | "tsv")) {
                let bytes = read_capped(&path, DOC_MAX)
                    .map_err(|why| ToolError::invalid(format!("{file} {why}")))?;
                let from = path.to_string_lossy().into_owned();
                return Ok(json!({ "shown": title, "bytes": bytes.len(), "file": from }));
            }
            if !matches!(kind.as_deref(), Some("html" | "htm" | "svg")) {
                return Err(ToolError::invalid(format!(
                    "{file} is no .html, .htm, .svg, .pdf, .csv or .tsv file"
                )));
            }
            let bytes = read_capped(&path, max)
                .map_err(|why| ToolError::invalid(format!("{file} {why}")))?;
            let text = String::from_utf8(bytes).map_err(|e| {
                ToolError::invalid(format!("{file} is not UTF-8 text: {}", e.utf8_error()))
            })?;
            (text, Some(path.to_string_lossy().into_owned()))
        }
        (Some(_), Some(_)) => return Err(ToolError::invalid("give `html` or `file`, not both")),
        (None, None) => return Err(ToolError::invalid("give the page as `html` or `file`")),
    };
    let remote = remote_loads(&text);
    if !remote.is_empty() {
        return Err(ToolError::invalid(format!(
            "the page has no network, so it cannot load {}; put them inline",
            remote.join(", ")
        )));
    }
    Ok(json!({ "shown": title, "bytes": text.len(), "file": from }))
}

/// What a page would load from the network: the http(s) and protocol-relative URLs in its
/// `src`, `href` (of a stylesheet or script), `srcset`, CSS `url()` and `@import`; the first
/// five, in order.
fn remote_loads(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    for marker in ["src=", "href=", "url(", "@import"] {
        let mut from = 0;
        while let Some(at) = lower.get(from..).and_then(|rest| rest.find(marker)) {
            let start = from.saturating_add(at).saturating_add(marker.len());
            from = start;
            let rest = lower.get(start..).unwrap_or_default().trim_start_matches([' ', '"', '\'']);
            // A link to read is no load: an anchor's `href` takes the person somewhere else.
            if marker == "href=" && !loads_by_href(&lower, start) {
                continue;
            }
            if rest.starts_with("http://") || rest.starts_with("https://") || rest.starts_with("//")
            {
                let skipped =
                    lower.get(start..).unwrap_or_default().len().saturating_sub(rest.len());
                let offset = start.saturating_add(skipped);
                let url: String = html
                    .get(offset..)
                    .unwrap_or_default()
                    .chars()
                    .take_while(|c| !matches!(c, '"' | '\'' | ')' | ' ' | '>' | ';'))
                    .collect();
                if !found.contains(&url) {
                    found.push(url);
                }
            }
        }
    }
    found.truncate(5);
    found
}

/// Whether the `href` at `at` in `lower` is a `<link>`'s: a stylesheet, a font, a preload.
fn loads_by_href(lower: &str, at: usize) -> bool {
    let before = lower.get(..at).unwrap_or_default();
    let tag = before.rfind('<').and_then(|open| before.get(open..)).unwrap_or_default();
    tag.starts_with("<link")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visual_of(
        script: &str,
        assets: BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<Value, ToolError> {
        visual(ShowVisualArgs { script: script.to_owned(), title: "Bars".to_owned(), assets }, cwd)
    }

    /// A two-series bar chart passes the sandbox's check, its controls and nodes counted.
    #[cfg(feature = "visual")]
    #[test]
    fn a_visual_that_runs_is_shown() {
        let script = "\
local data = { { r = \"a\", x = 1, y = 2 }, { r = \"b\", x = 3, y = 4 } }
return function()
  return ui.chart { type = \"bar\", height = 200, x = ui.map(data, function(d) return d.r end),
    series = { { name = \"x\", values = ui.map(data, function(d) return d.x end) },
               { name = \"y\", values = ui.map(data, function(d) return d.y end) } } }
end";
        let shown = visual_of(script, BTreeMap::new(), Path::new("/")).unwrap();
        assert_eq!(shown["shown"], json!("Bars"));
        assert!(shown["nodes"].as_u64().unwrap() >= 1, "{shown}");
    }

    /// A script that fails comes back with its line, for the agent to fix.
    #[cfg(feature = "visual")]
    #[test]
    fn a_failing_script_comes_back_with_its_line() {
        let script = "local x = nil\nlocal y = 1\nx()\nreturn function() return ui.text \"hi\" end";
        let e = visual_of(script, BTreeMap::new(), Path::new("/")).unwrap_err();
        assert!(e.message.contains("line 3:"), "{}", e.message);
    }

    /// Without the sandbox, a visual is refused with where it runs.
    #[cfg(not(feature = "visual"))]
    #[test]
    fn without_the_sandbox_a_visual_says_where_it_runs() {
        let e = visual_of("return function() end", BTreeMap::new(), Path::new("/")).unwrap_err();
        assert!(e.message.contains("slopty mcp"), "{}", e.message);
    }

    /// Assets are plain names, distinct by more than case, files that are there, within size;
    /// a script over its cap is refused before it runs.
    #[test]
    fn assets_and_scripts_are_held_to_their_limits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"png").unwrap();
        let named = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(n, p)| ((*n).to_owned(), (*p).to_owned())).collect()
        };
        let read = read_assets(&named(&[("a.png", "a.png")]), dir.path()).unwrap();
        assert_eq!(read, vec![("a.png".to_owned(), b"png".to_vec())]);
        let bad = read_assets(&named(&[("../a.png", "a.png")]), dir.path()).unwrap_err();
        assert!(bad.message.contains("plain file name"), "{}", bad.message);
        let twice = read_assets(&named(&[("A.png", "a.png"), ("a.png", "a.png")]), dir.path());
        assert!(twice.unwrap_err().message.contains("more than case"));
        let missing = read_assets(&named(&[("b.png", "b.png")]), dir.path()).unwrap_err();
        assert!(missing.message.contains("can't be read"), "{}", missing.message);
        let folder = read_assets(&named(&[("d", ".")]), dir.path()).unwrap_err();
        assert!(folder.message.contains("not a file"), "{}", folder.message);
        let big = "-".repeat(SCRIPT_MAX + 1);
        assert!(visual_of(&big, BTreeMap::new(), dir.path()).unwrap_err().message.contains("KB"));
    }

    fn page_of(html: Option<&str>, file: Option<&str>, cwd: &Path) -> Result<Value, ToolError> {
        let args = ShowPageArgs {
            html: html.map(str::to_owned),
            file: file.map(str::to_owned),
            title: "Page".to_owned(),
        };
        page(args, cwd)
    }

    /// A page is HTML or a file of HTML or SVG; what it would load from the network is named
    /// back, a link to read is not.
    #[test]
    fn a_page_is_checked_for_what_it_cannot_load() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path();
        let inline =
            "<p style=\"color: var(--color-text-primary)\">hi</p><a href=\"https://x.dev\">x</a>";
        assert_eq!(page_of(Some(inline), None, here).unwrap()["shown"], json!("Page"));
        let remote = "<script src=\"https://cdn.example/d3.js\"></script>\
            <link rel=\"stylesheet\" href=\"//fonts.example/f.css\"><style>@import 'https://a.b/c.css';</style>";
        let e = page_of(Some(remote), None, here).unwrap_err();
        assert!(e.message.contains("https://cdn.example/d3.js"), "{}", e.message);
        assert!(e.message.contains("//fonts.example/f.css"), "{}", e.message);
        assert!(e.message.contains("https://a.b/c.css"), "{}", e.message);
        std::fs::write(here.join("chart.svg"), "<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .unwrap();
        let shown = page_of(None, Some("chart.svg"), here).unwrap();
        assert_eq!(shown["file"], json!(here.join("chart.svg").to_string_lossy()));
        std::fs::write(here.join("latency.csv"), "region,p99\neu,120\n").unwrap();
        assert_eq!(page_of(None, Some("latency.csv"), here).unwrap()["bytes"], json!(18));
        std::fs::write(here.join("notes.txt"), "hi").unwrap();
        assert!(page_of(None, Some("notes.txt"), here).is_err(), "no page");
        assert!(page_of(Some("<p/>"), Some("chart.svg"), here).is_err(), "not both");
        assert!(page_of(None, None, here).is_err(), "one of them");
        assert!(page_of(None, Some("gone.html"), here).is_err(), "a missing file");
        let big = "x".repeat(PAGE_MAX + 1);
        assert!(page_of(Some(&big), None, here).unwrap_err().message.contains("KB"));
    }
}
