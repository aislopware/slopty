//! The chart libraries a page may load, and the pages chart fences show as.
//!
//! The libraries are vendored in the app and served from the page's own origin under
//! [`PREFIX`], so a chart never reaches a CDN. A fenced `vega-lite`, `vega` or `plotly` block of
//! a reply shows as a page of its own ([`fence_document`]).
//!
//! The files are upstream's builds byte for byte (`assets/page-libs/`; versions, licences and
//! sizes in `THIRD_PARTY_NOTICES.md` and `docs/MEASUREMENTS.md`). The one change is served, not
//! stored: `vega-interpreter` ships as an ES module, so it is wrapped to read `vega-util` from
//! Vega's own global and to set `vegaInterpreter`, as a classic script can load it. Vega
//! evaluates its expressions with that interpreter rather than `Function`, which a page's policy
//! would refuse (`script-src` has no `'unsafe-eval'`).
//!
//! A chart page reports what it drew through MCP Apps' bridge, as a log for the host
//! ([`PageEvent::Log`](super::PageEvent::Log), logger [`CHART_LOGGER`]): for Vega, the
//! number of rows of each of its data sets; for Plotly, the points of each trace. It asks for
//! its own height (`ui/notifications/size-changed`) once drawn and whenever it changes.

use std::borrow::Cow;
use std::fmt::Write as _;

use super::Resource;

/// Where a page finds the libraries: `<script src=".slopty/lib/vega.min.js">`, resolved
/// against the page's own origin.
pub const PREFIX: &str = ".slopty/lib/";

/// The logger name a chart page reports what it drew under.
pub const CHART_LOGGER: &str = "slopty-chart";

/// One vendored library.
#[derive(Clone, Copy, Debug)]
pub struct Lib {
    /// Its file name under [`PREFIX`].
    pub file: &'static str,
    /// Its npm package.
    pub package: &'static str,
    /// The package's version.
    pub version: &'static str,
    /// Its licence (SPDX).
    pub licence: &'static str,
    /// The file, as upstream builds it.
    pub bytes: &'static [u8],
}

/// Every library a page may load.
pub const LIBS: [Lib; 5] = [
    Lib {
        file: "vega.min.js",
        package: "vega",
        version: "6.4.0",
        licence: "BSD-3-Clause",
        bytes: include_bytes!("../../../assets/page-libs/vega.min.js"),
    },
    Lib {
        file: "vega-lite.min.js",
        package: "vega-lite",
        version: "6.5.0",
        licence: "BSD-3-Clause",
        bytes: include_bytes!("../../../assets/page-libs/vega-lite.min.js"),
    },
    Lib {
        file: "vega-embed.min.js",
        package: "vega-embed",
        version: "7.3.0",
        licence: "BSD-3-Clause",
        bytes: include_bytes!("../../../assets/page-libs/vega-embed.min.js"),
    },
    Lib {
        file: "vega-interpreter.js",
        package: "vega-interpreter",
        version: "2.3.2",
        licence: "BSD-3-Clause",
        bytes: include_bytes!("../../../assets/page-libs/vega-interpreter.js"),
    },
    Lib {
        file: "plotly-cartesian.min.js",
        package: "plotly.js-cartesian-dist-min",
        version: "4.1.2",
        licence: "MIT",
        #[expect(
            clippy::large_include_file,
            reason = "Plotly's cartesian bundle, 1.5 MB, which a chart page loads whole"
        )]
        bytes: include_bytes!("../../../assets/page-libs/plotly-cartesian.min.js"),
    },
];

/// The library a page asked for at `path` (relative to its origin), as served; `None` for a
/// path not under [`PREFIX`] or no such library.
#[must_use]
pub fn lib(path: &str) -> Option<Resource> {
    let file = path.strip_prefix(PREFIX)?;
    let lib = LIBS.iter().find(|lib| lib.file == file)?;
    let bytes = match served(lib) {
        Cow::Borrowed(bytes) => bytes.to_vec(),
        Cow::Owned(bytes) => bytes,
    };
    Some(Resource { mime: "text/javascript; charset=utf-8".to_owned(), bytes })
}

/// A library as a classic script loads it.
fn served(lib: &Lib) -> Cow<'static, [u8]> {
    if lib.file != "vega-interpreter.js" {
        return Cow::Borrowed(lib.bytes);
    }
    let source = String::from_utf8_lossy(lib.bytes);
    let body = source
        .lines()
        .filter(|line| !line.starts_with("import ") && !line.starts_with("export "))
        .fold(String::new(), |mut body, line| {
            body.push_str(line);
            body.push('\n');
            body
        });
    Cow::Owned(
        format!(
            "(function (vegaUtil) {{\nconst {{ ascending, isString, DisallowedObjectProperties }} \
             = vegaUtil;\n{body}self.vegaInterpreter = {{ expressionInterpreter: expression }};\n\
             }})(self.vega);\n"
        )
        .into_bytes(),
    )
}

/// What a fenced block's language makes of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chart {
    /// A Vega-Lite specification.
    VegaLite,
    /// A Vega specification.
    Vega,
    /// A Plotly figure: `{ data, layout }`, or its traces alone.
    Plotly,
}

impl Chart {
    /// The chart a fence's language names: `vega-lite` (or `vegalite`, `vl`), `vega`, `plotly`.
    #[must_use]
    pub fn of(language: &str) -> Option<Self> {
        match language.trim().to_ascii_lowercase().as_str() {
            "vega-lite" | "vegalite" | "vl" => Some(Self::VegaLite),
            "vega" => Some(Self::Vega),
            "plotly" | "plotly.js" | "plotly-json" => Some(Self::Plotly),
            _ => None,
        }
    }

    /// Its name in the card's header.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::VegaLite => "Vega-Lite",
            Self::Vega => "Vega",
            Self::Plotly => "Plotly",
        }
    }
}

/// The page a fenced block of `chart` JSON shows as.
///
/// It loads the libraries from the page's own origin, holds the specification as data, and
/// draws it in the theme's variables, reporting what it drew. `None` when `source` is not a
/// JSON object (or, for Plotly, an array of traces): the fence stays code.
#[must_use]
pub fn fence_document(chart: Chart, source: &str) -> Option<String> {
    let spec: serde_json::Value = serde_json::from_str(source).ok()?;
    let fits = match chart {
        Chart::VegaLite | Chart::Vega => spec.is_object(),
        Chart::Plotly => spec.is_object() || spec.is_array(),
    };
    if !fits {
        return None;
    }
    // Inside a script element, `</` and `<!--` would end it or start a comment; JSON reads
    // `<\/` as `</`.
    let data = spec.to_string().replace("</", "<\\/").replace("<!--", "<\\u0021--");
    let (scripts, draw) = match chart {
        Chart::VegaLite | Chart::Vega => (
            ["vega.min.js", "vega-lite.min.js", "vega-interpreter.js", "vega-embed.min.js"]
                .as_slice(),
            VEGA_DRAW,
        ),
        Chart::Plotly => (["plotly-cartesian.min.js"].as_slice(), PLOTLY_DRAW),
    };
    let scripts = scripts.iter().fold(String::new(), |mut all, file| {
        let _wrote = write!(all, "<script src=\"{PREFIX}{file}\"></script>");
        all
    });
    let mode = match chart {
        Chart::VegaLite => "vega-lite",
        Chart::Vega => "vega",
        Chart::Plotly => "plotly",
    };
    Some(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>{STYLE}</style>{scripts}\
         </head><body><div id=\"chart\"></div>\
         <script type=\"application/json\" id=\"spec\">{data}</script>\
         <script>const SLOPTY_MODE = \"{mode}\";\n{BRIDGE}\n{draw}</script></body></html>"
    ))
}

/// The page's own style: the chart on the theme's surface, its text in the theme's faces.
const STYLE: &str = "html,body{margin:0;padding:0;background:var(--color-background-primary,\
transparent);color:var(--color-text-primary,currentColor);font-family:var(--font-sans,\
system-ui)}#chart{width:100%}.vega-embed{width:100%;display:block}\
.vega-embed summary{display:none}";

/// What every chart page says to its host: its height and what it drew.
const BRIDGE: &str = r##"const slopty = (() => {
  const post = (message) => window.parent.postMessage(Object.assign({ jsonrpc: "2.0" }, message), "*");
  const css = (name, fallback) => getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
  let last = 0;
  const size = () => {
    const height = Math.ceil(document.getElementById("chart").getBoundingClientRect().height);
    if (height > 0 && height !== last) {
      last = height;
      post({ method: "ui/notifications/size-changed", params: { width: document.documentElement.clientWidth, height } });
    }
  };
  const report = (data) => post({ method: "notifications/message", params: { level: "info", logger: "slopty-chart", data } });
  const series = [1, 2, 3, 4, 5, 6].map((n) => css("--color-chart-" + n, "")).filter(Boolean);
  return {
    size,
    report,
    watch: () => new ResizeObserver(size).observe(document.getElementById("chart")),
    theme: {
      background: css("--color-background-primary", "transparent"),
      text: css("--color-text-primary", "#222"),
      muted: css("--color-text-secondary", "#666"),
      border: css("--color-border-primary", "#ddd"),
      grid: css("--color-border-secondary", css("--color-border-primary", "#eee")),
      font: css("--font-sans", "system-ui"),
      series: series.length ? series : undefined,
    },
  };
})();
const spec = JSON.parse(document.getElementById("spec").textContent);"##;

/// Draws a Vega or Vega-Lite specification with vega-embed, in the theme.
const VEGA_DRAW: &str = r##"(async () => {
  const t = slopty.theme;
  const config = {
    background: t.background,
    font: t.font,
    view: { stroke: null },
    axis: { domainColor: t.border, gridColor: t.grid, tickColor: t.border, labelColor: t.muted, titleColor: t.muted, labelFontSize: 11, titleFontSize: 11, titleFontWeight: 500 },
    legend: { labelColor: t.muted, titleColor: t.muted, labelFontSize: 11, titleFontSize: 11, titleFontWeight: 500 },
    title: { color: t.text, fontSize: 13, fontWeight: 600, anchor: "start" },
    header: { labelColor: t.muted, titleColor: t.muted },
    style: { "guide-label": { fill: t.muted }, "guide-title": { fill: t.muted } },
  };
  if (t.series) {
    config.range = { category: t.series };
    config.mark = { color: t.series[0] };
  }
  if (SLOPTY_MODE === "vega-lite" && spec.width === undefined && (spec.mark || spec.layer)) {
    spec.width = "container";
  }
  try {
    const result = await vegaEmbed("#chart", spec, {
      mode: SLOPTY_MODE,
      actions: false,
      renderer: "svg",
      config,
      ast: true,
      expr: vegaInterpreter.expressionInterpreter,
    });
    const view = result.view;
    const data = {};
    for (const name of Object.keys(view._runtime.data || {})) {
      try {
        const rows = view.data(name);
        if (Array.isArray(rows)) data[name] = rows.length;
      } catch (_) {}
    }
    slopty.size();
    slopty.watch();
    slopty.report({ chart: SLOPTY_MODE, data });
  } catch (error) {
    slopty.report({ chart: SLOPTY_MODE, error: String(error && error.message || error) });
  }
})();"##;

/// Draws a Plotly figure, in the theme.
const PLOTLY_DRAW: &str = r#"(async () => {
  const t = slopty.theme;
  const figure = Array.isArray(spec) ? { data: spec } : spec;
  const axis = { gridcolor: t.grid, linecolor: t.border, zerolinecolor: t.border, tickcolor: t.border };
  const layout = Object.assign({ height: 320 }, figure.layout || {}, {
    paper_bgcolor: t.background,
    plot_bgcolor: t.background,
    font: Object.assign({ family: t.font, color: t.muted, size: 11 }, (figure.layout || {}).font || {}),
    margin: Object.assign({ l: 48, r: 16, t: (figure.layout || {}).title ? 40 : 16, b: 40 }, (figure.layout || {}).margin || {}),
  });
  layout.xaxis = Object.assign({}, axis, layout.xaxis || {});
  layout.yaxis = Object.assign({}, axis, layout.yaxis || {});
  if (t.series && !layout.colorway) layout.colorway = t.series;
  try {
    await Plotly.newPlot("chart", figure.data || [], layout, { displayModeBar: false, responsive: true });
    const traces = (figure.data || []).map((trace) => {
      const points = trace.y || trace.x || trace.values || trace.z || [];
      return Array.isArray(points) ? points.length : 0;
    });
    slopty.size();
    slopty.watch();
    slopty.report({ chart: "plotly", traces });
  } catch (error) {
    slopty.report({ chart: "plotly", error: String(error && error.message || error) });
  }
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Each library is served from under the prefix as JavaScript, and nothing else is.
    #[test]
    fn libraries_are_served_from_the_pages_own_origin() {
        for vendored in LIBS {
            let served = lib(&format!("{PREFIX}{}", vendored.file)).expect(vendored.file);
            assert!(served.mime.starts_with("text/javascript"));
            assert!(served.bytes.len() > 1000, "{}", vendored.file);
        }
        assert!(lib("vega.min.js").is_none(), "only under the prefix");
        assert!(lib(&format!("{PREFIX}../secret")).is_none());
        assert!(lib(&format!("{PREFIX}leaflet.js")).is_none());
    }

    /// The interpreter is served as a classic script that reads Vega's global; the rest are
    /// upstream's bytes.
    #[test]
    fn the_interpreter_is_wrapped_as_a_classic_script() {
        let wrapped = lib(&format!("{PREFIX}vega-interpreter.js")).expect("served");
        let text = String::from_utf8(wrapped.bytes).expect("text");
        assert!(!text.lines().any(|l| l.starts_with("import ") || l.starts_with("export ")));
        assert!(text.contains("self.vegaInterpreter = { expressionInterpreter: expression }"));
        assert!(text.trim_end().ends_with("})(self.vega);"));
        let vega = lib(&format!("{PREFIX}vega.min.js")).expect("served");
        assert_eq!(vega.bytes, LIBS[0].bytes);
    }

    /// A fence's language names its chart.
    #[test]
    fn a_fences_language_names_its_chart() {
        assert_eq!(Chart::of("vega-lite"), Some(Chart::VegaLite));
        assert_eq!(Chart::of(" VL "), Some(Chart::VegaLite));
        assert_eq!(Chart::of("vega"), Some(Chart::Vega));
        assert_eq!(Chart::of("plotly"), Some(Chart::Plotly));
        assert_eq!(Chart::of("json"), None);
    }

    /// A specification becomes a page that loads the libraries from its own origin and no
    /// other, holds the specification as data that cannot end its script, and is refused when
    /// it is not JSON.
    #[test]
    fn a_fence_becomes_a_page_of_its_own() {
        let spec =
            r#"{"data":{"values":[{"a":"</script><script>alert(1)</script>"}]},"mark":"bar"}"#;
        let page = fence_document(Chart::VegaLite, spec).expect("a page");
        assert!(page.contains("<script src=\".slopty/lib/vega.min.js\"></script>"));
        assert!(!page.contains("http"), "no address beyond the page's own origin");
        assert_eq!(page.matches("</script>").count(), 6, "four libraries, the data, the draw");
        let data = page.split("id=\"spec\">").nth(1).and_then(|s| s.split("</script>").next());
        let back: serde_json::Value = serde_json::from_str(data.expect("data")).expect("json");
        assert_eq!(back["data"]["values"][0]["a"], "</script><script>alert(1)</script>");
        assert!(fence_document(Chart::VegaLite, "{\"mark\": ").is_none(), "half written");
        assert!(fence_document(Chart::VegaLite, "[1, 2]").is_none());
        let plotly = fence_document(Chart::Plotly, r#"[{"y":[1,2,3]}]"#).expect("traces");
        assert!(plotly.contains("plotly-cartesian.min.js"));
        assert!(!plotly.contains("vega.min.js"));
    }
}
