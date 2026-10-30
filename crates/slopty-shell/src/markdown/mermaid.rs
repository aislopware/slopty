// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Isolated native Mermaid adapter. Call only on a background executor.
use crate::theme::Theme;
use std::sync::Mutex;

pub const ENGINE_VERSION: &str = "merman/0.9"; // merman draws the diagrams.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024;
// Bound inputs and serialize the engine's CPU work across previews; UI owners discard results
// from superseded revisions.
static RENDER_LOCK: Mutex<()> = Mutex::new(());

/// Slopty's diagram style. The canvas is left transparent so the diagram sits
/// on its fence body; `canvas` is that body's opaque approximation, used where
/// the engine needs a solid mask (edge label pills, hollow markers).
#[derive(Clone)]
pub struct Palette {
    dark: bool,
    font: String,
    canvas: String,
    node: String,
    group: String,
    text: String,
    label: String,
    line: String,
    border: String,
    grid: String,
    accent_line: String,
    accent_wash: String,
}

fn color(color: gpui::Hsla, background: gpui::Hsla) -> String {
    let mut c = color.to_rgb();
    let bg = background.to_rgb();
    c.r = c.r * c.a + bg.r * (1.0 - c.a);
    c.g = c.g * c.a + bg.g * (1.0 - c.a);
    c.b = c.b * c.a + bg.b * (1.0 - c.a);
    format!(
        "#{:02x}{:02x}{:02x}",
        (c.r * 255.0).round() as u8,
        (c.g * 255.0).round() as u8,
        (c.b * 255.0).round() as u8
    )
}

impl Palette {
    pub fn from_theme(theme: &Theme) -> Self {
        let dark = theme.appearance.is_dark();
        let canvas = Self::plate(theme);
        // Nodes are cards lifted off the fence: white in light, one ink step
        // up in dark, where the panel is already the deepest plane.
        let node = if dark {
            canvas.blend(theme.ink(0.06))
        } else {
            theme.bg
        };
        Self {
            dark,
            font: theme.font_sans.to_string(),
            canvas: color(canvas, theme.bg),
            node: color(node, canvas),
            group: color(theme.ink(0.03), canvas),
            text: color(theme.text, node),
            label: color(theme.text_muted, canvas),
            line: color(theme.text_faint, canvas),
            border: color(theme.border_strong, canvas),
            grid: color(theme.border, canvas),
            accent_line: color(theme.accent.opacity(0.6), canvas),
            accent_wash: color(theme.accent.opacity(0.12), node),
        }
    }

    /// The fence body, an ink wash over the panel (see `code_block_frame`).
    /// Also the fill behind a diagram shown off its fence, as in the lightbox.
    pub fn plate(theme: &Theme) -> gpui::Hsla {
        theme.bg.blend(theme.ink(0.035))
    }
}

// Merman draws the diagrams (Mermaid 12, Zed's choice), measuring labels with the fonts
// usvg outlines them in, so no label overflows its box (`docs/decisions/ui.md`, "Mermaid through
// merman").
pub fn render(source: &str, palette: &Palette) -> Result<String, String> {
    if source.len() > MAX_SOURCE_BYTES
        || source.lines().count() > 256
        || source
            .split(|c: char| c.is_whitespace() || matches!(c, ';' | '>' | '{' | '}'))
            .count()
            > 2048
    {
        return Err("Diagram exceeds preview complexity limit".into());
    }
    let _guard = RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::panic::catch_unwind(|| {
        let renderer = merman::Renderer::new()
            .with_engine(merman::Engine::new().with_site_config(site_config(palette)));
        let request = merman::SvgRequest {
            environment: merman::SvgEnvironment::deterministic()
                .with_text_measurement_policy(measurement(&palette.font)?),
            // usvg draws no HTML labels: merman's resvg-safe pipeline sets them as wrapped SVG
            // text, then Slopty's touches the theme cannot say.
            pipeline: Some(
                merman::svg::SvgPipeline::resvg_safe()
                    .with_postprocessor(merman::svg::ScopedCssPostprocessor::new(touches(palette))),
            ),
            options: merman::svg::SvgRenderOptions {
                diagram_id: Some("slopty-mermaid".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        };
        let output = renderer
            .render(merman::RenderRequest::svg(
                source,
                merman::OperationControl::new(),
                request,
            ))
            .map_err(|e| e.to_string())?;
        let merman::RenderOutput::Svg(Some(svg)) = output else {
            return Err("Not a Mermaid diagram".to_owned());
        };
        let svg = svg.svg().to_owned();
        if svg.len() > 2 * 1024 * 1024 {
            return Err("Diagram output exceeds preview size limit".into());
        }
        Ok(svg)
    })
    .unwrap_or_else(|_| Err("Diagram could not be rendered".into()))
}

/// Slopty's style as Mermaid's own theme: the palette's colours on the `base` theme, a denser
/// layout than its default (diagrams scale to the reading column, so slack spacing shrinks the
/// labels).
fn site_config(palette: &Palette) -> merman::MermaidConfig {
    let p = palette;
    let colors: Vec<(&str, &str)> = vec![
        ("fontFamily", p.font.as_str()),
        ("fontSize", "14px"),
        ("background", p.canvas.as_str()),
        ("primaryColor", p.node.as_str()),
        ("primaryTextColor", p.text.as_str()),
        ("primaryBorderColor", p.border.as_str()),
        ("secondaryColor", p.node.as_str()),
        ("secondaryTextColor", p.text.as_str()),
        ("tertiaryColor", p.group.as_str()),
        ("tertiaryTextColor", p.text.as_str()),
        ("mainBkg", p.node.as_str()),
        ("nodeBorder", p.border.as_str()),
        ("nodeTextColor", p.text.as_str()),
        ("lineColor", p.line.as_str()),
        ("textColor", p.label.as_str()),
        ("titleColor", p.text.as_str()),
        ("labelTextColor", p.text.as_str()),
        ("labelColor", p.text.as_str()),
        ("edgeLabelBackground", p.canvas.as_str()),
        ("clusterBkg", p.group.as_str()),
        ("clusterBorder", p.border.as_str()),
        ("noteBkgColor", p.accent_wash.as_str()),
        ("noteBorderColor", p.accent_line.as_str()),
        ("noteTextColor", p.text.as_str()),
        ("actorBkg", p.node.as_str()),
        ("actorBorder", p.border.as_str()),
        ("actorTextColor", p.text.as_str()),
        ("actorLineColor", p.border.as_str()),
        ("signalColor", p.line.as_str()),
        ("signalTextColor", p.text.as_str()),
        ("loopTextColor", p.text.as_str()),
        ("activationBkgColor", p.accent_wash.as_str()),
        ("activationBorderColor", p.accent_line.as_str()),
        ("classText", p.text.as_str()),
        ("attributeBackgroundColorOdd", p.node.as_str()),
        ("attributeBackgroundColorEven", p.group.as_str()),
        ("gridColor", p.grid.as_str()),
        ("sectionBkgColor", p.group.as_str()),
        ("altSectionBkgColor", p.canvas.as_str()),
        ("sectionBkgColor2", p.group.as_str()),
        ("taskBkgColor", p.accent_wash.as_str()),
        ("taskBorderColor", p.accent_line.as_str()),
        ("taskTextColor", p.text.as_str()),
        ("taskTextLightColor", p.text.as_str()),
        ("taskTextOutsideColor", p.label.as_str()),
        ("activeTaskBkgColor", p.accent_wash.as_str()),
        ("activeTaskBorderColor", p.accent_line.as_str()),
        ("doneTaskBkgColor", p.group.as_str()),
        ("doneTaskBorderColor", p.border.as_str()),
        ("todayLineColor", p.accent_line.as_str()),
        ("pieTitleTextColor", p.text.as_str()),
        ("pieSectionTextColor", p.text.as_str()),
        ("pieLegendTextColor", p.text.as_str()),
        ("pieStrokeColor", p.border.as_str()),
        ("pieOuterStrokeColor", p.border.as_str()),
    ];
    let mut variables: serde_json::Map<String, serde_json::Value> = colors
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.into()))
        .collect();
    variables.insert("darkMode".to_owned(), p.dark.into());
    merman::MermaidConfig::from_value(serde_json::json!({
        "theme": "base",
        "darkMode": p.dark,
        "fontFamily": p.font,
        "fontSize": 14,
        "htmlLabels": true,
        "flowchart": {
            "htmlLabels": true,
            "padding": 14,
            "nodeSpacing": 36,
            "rankSpacing": 40,
            "wrappingWidth": 240,
        },
        "themeVariables": variables,
    }))
}

/// What Mermaid's theme cannot say: softer node corners and decisions in the accent. Plain
/// selectors without `!important`, so a diagram's own `style` and `classDef` colours win.
fn touches(palette: &Palette) -> String {
    format!(
        ".node rect {{ rx: 8px; ry: 8px; }}\n\
         .node polygon {{ fill: {wash}; stroke: {line}; }}\n",
        wash = palette.accent_wash,
        line = palette.accent_line,
    )
}

/// Labels measured with the face usvg will set them in (`image_media::svg_options` loads the
/// bundled Geist faces), so a box is as wide as its words.
fn measurement(family: &str) -> Result<merman::svg::TextMeasurementPolicy, String> {
    use merman::svg::{
        DeterministicTextMeasurer, MeasurementProfileId, TextMeasurementPolicy,
        TextMeasurementProfile, TextMeasurementProfileIdentity,
    };
    let mono = family.to_ascii_lowercase().contains("mono");
    let measurer = DeterministicTextMeasurer::default().with_width_callback(move |text, style| {
        let bold = style.font_weight.as_deref().is_some_and(|weight| {
            weight.eq_ignore_ascii_case("bold")
                || weight.eq_ignore_ascii_case("bolder")
                || weight.parse::<u16>().is_ok_and(|weight| weight >= 600)
        });
        text_width(text, style.font_size, mono, bold)
    });
    let identity = TextMeasurementProfileIdentity::new(
        MeasurementProfileId::new("slopty.bundled-geist").map_err(|e| e.to_string())?,
        if mono { "1:geist-mono" } else { "1:geist" },
    )
    .map_err(|e| e.to_string())?;
    Ok(TextMeasurementPolicy::uniform(TextMeasurementProfile::new(identity, measurer)))
}

/// A bundled face's advances, in ems per char.
struct Face {
    mono: bool,
    weight: u16,
    advances: std::collections::HashMap<char, f64>,
    fallback: f64,
}

fn faces() -> &'static [Face] {
    static FACES: std::sync::OnceLock<Vec<Face>> = std::sync::OnceLock::new();
    FACES.get_or_init(|| {
        crate::typography::bundled_font_faces()
            .filter_map(|data| {
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                let units = f64::from(face.units_per_em());
                let family = face
                    .names()
                    .into_iter()
                    .find(|name| name.name_id == ttf_parser::name_id::FAMILY && name.is_unicode())
                    .and_then(|name| name.to_string())
                    .unwrap_or_default();
                let advance = |c: char| {
                    let glyph = face.glyph_index(c)?;
                    Some(f64::from(face.glyph_hor_advance(glyph)?) / units)
                };
                let fallback = advance('n').unwrap_or(0.55);
                let advances = (' '..='~')
                    .chain('\u{a0}'..='\u{24f}')
                    .filter_map(|c| Some((c, advance(c)?)))
                    .collect();
                Some(Face {
                    mono: family.to_ascii_lowercase().contains("mono"),
                    weight: face.weight().to_number(),
                    advances,
                    fallback,
                })
            })
            .collect()
    })
}

/// `text`'s width in pixels at `size`, in the bundled face nearest `mono` and `bold`.
fn text_width(text: &str, size: f64, mono: bool, bold: bool) -> f64 {
    let want = if bold { 600 } else { 400 };
    let face = faces()
        .iter()
        .filter(|face| face.mono == mono)
        .min_by_key(|face| face.weight.abs_diff(want));
    let Some(face) = face else {
        return text.chars().count() as f64 * 0.6 * size;
    };
    text.chars()
        .map(|c| face.advances.get(&c).copied().unwrap_or(face.fallback))
        .sum::<f64>()
        * size
}

#[cfg(test)]
mod tests {
    use super::*;
    const CORPUS: &[(&str, &str)] = &[
        (
            "flowchart",
            include_str!("../../fixtures/markdown-preview/flowchart.mmd"),
        ),
        (
            "sequence",
            include_str!("../../fixtures/markdown-preview/sequence.mmd"),
        ),
        (
            "class",
            include_str!("../../fixtures/markdown-preview/class.mmd"),
        ),
        (
            "state",
            include_str!("../../fixtures/markdown-preview/state.mmd"),
        ),
        (
            "er",
            include_str!("../../fixtures/markdown-preview/er.mmd"),
        ),
        (
            "gantt",
            include_str!("../../fixtures/markdown-preview/gantt.mmd"),
        ),
    ];
    #[test]
    fn corpus_renders_through_gpui_in_both_themes() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(crate::icons::Assets));
        let mut light_mono = Theme::light();
        light_mono.font_sans = "Geist Mono".into();
        let mut dark_mono = Theme::dark();
        dark_mono.font_sans = "Geist Mono".into();
        for (mode, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("light-mono", light_mono),
            ("dark-mono", dark_mono),
        ] {
            let palette = Palette::from_theme(&theme);
            for (name, source) in CORPUS {
                let svg = render(source, &palette).unwrap_or_else(|e| panic!("{mode}/{name}: {e}"));
                assert!(!svg.contains("<foreignObject"));
                let raster = renderer.render_single_frame(svg.as_bytes(), 1.0).unwrap();
                assert!(raster.size(0).width.0 > 0);
                let prepared =
                    crate::image_media::decode_image("image/svg+xml", svg.as_bytes().to_vec())
                        .unwrap();
                let prepared_raster = prepared
                    .image
                    .to_image_data(gpui::SvgRenderer::new(std::sync::Arc::new(
                        crate::icons::Assets,
                    )))
                    .unwrap();
                let size = prepared_raster.size(0);
                assert!(size.width.0 > 0 && size.height.0 > 0);
                assert!(size.width.0 <= 4096 && size.height.0 <= 4096);
                assert!(size.width.0 as usize * size.height.0 as usize <= 1024 * 1024);
                let ratio = size.width.0 as f32 / size.height.0 as f32;
                assert!((ratio / (prepared.width / prepared.height) - 1.0).abs() < 0.02);
                assert_eq!(svg, render(source, &palette).unwrap());
                if let Ok(dir) = std::env::var("SLOPTY_MERMAID_ARTIFACTS") {
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(format!("{dir}/{name}-{mode}.svg"), svg).unwrap();
                    let size = prepared_raster.size(0);
                    let mut rgba = prepared_raster.as_bytes(0).unwrap().to_vec();
                    for pixel in rgba.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                    }
                    image::save_buffer(
                        format!("{dir}/{name}-{mode}.png"),
                        &rgba,
                        size.width.0 as u32,
                        size.height.0 as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        }
    }
    #[test]
    fn malformed_and_oversized_diagrams_are_recoverable() {
        let palette = Palette::from_theme(&Theme::dark());
        assert!(render("this is not a diagram", &palette).is_err());
        assert!(render(&"x".repeat(MAX_SOURCE_BYTES + 1), &palette).is_err());
        assert!(render("flowchart TD\nA[Hola<br/>mundo] --> B[Fin]", &palette).is_ok());
    }

    // Merman's output, themed by Slopty.
    #[test]
    fn diagrams_take_slopty_style_and_keep_explicit_colors() {
        for theme in [Theme::light(), Theme::dark()] {
            let palette = Palette::from_theme(&theme);
            let svg = render(
                "flowchart TD\nA[Inicio] --> B{¿Listo?}\nB --> C[Fin]\nB --> D{Otra}\nstyle D fill:#dbeafe,stroke:#2563eb",
                &palette,
            )
            .unwrap();
            assert!(svg.contains(&palette.node), "nodes take the palette");
            assert!(svg.contains(&palette.accent_wash), "decisions take the accent");
            assert!(svg.to_ascii_lowercase().contains("#dbeafe"), "a diagram's own style stays");
        }
    }

    /// A long label wraps inside its node: every line usvg sets in the bundled face lies within
    /// its node's box (zed#65192).
    #[test]
    fn long_labels_stay_inside_their_nodes() {
        let palette = Palette::from_theme(&Theme::light());
        let svg = render(
            "flowchart TD\n    A[\"Pass 2: search the transcript with annotation blocks excised, then map \
             offsets back to buffer space\"] --> B[\"Error describing where the matches were found\"]",
            &palette,
        )
        .unwrap();
        let tree = usvg::Tree::from_str(&svg, &crate::image_media::svg_options()).unwrap();
        fn walk(group: &usvg::Group, boxes: &mut Vec<usvg::Rect>, lines: &mut Vec<usvg::Rect>) {
            for child in group.children() {
                match child {
                    usvg::Node::Group(inner) => {
                        if inner.id().contains("flowchart-") {
                            boxes.extend(inner.children().iter().find_map(|node| match node {
                                usvg::Node::Path(path) => Some(path.abs_bounding_box()),
                                _ => None,
                            }));
                        }
                        walk(inner, boxes, lines);
                    }
                    usvg::Node::Text(text) => lines.push(text.abs_bounding_box()),
                    _ => {}
                }
            }
        }
        let (mut boxes, mut lines) = (Vec::new(), Vec::new());
        walk(tree.root(), &mut boxes, &mut lines);
        assert_eq!(boxes.len(), 2, "both nodes: {svg}");
        assert!(lines.len() >= 4, "the long label wraps: {}", lines.len());
        for line in &lines {
            let middle = (line.top() + line.bottom()) / 2.0;
            let node = boxes
                .iter()
                .find(|node| node.top() <= middle && middle <= node.bottom())
                .unwrap_or_else(|| panic!("a line outside every node: {line:?}"));
            assert!(
                node.left() <= line.left() + 0.5 && line.right() <= node.right() + 0.5,
                "a line {line:?} wider than its node {node:?}"
            );
        }
    }
}
