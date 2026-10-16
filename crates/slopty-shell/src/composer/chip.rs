// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! What a chip looks like: its pill, icon and hover preview.
use super::*;

/// A staged attachment an attachment chip can refer to; an image chip opens
/// its picture on click.
pub(super) struct ChipAttachment {
    pub(super) image: Option<std::sync::Arc<gpui::Image>>,
}

/// Spaces reserve room at the start of every chip for its icon well, which is
/// painted over them, so the text, caret and wrapping all treat the chip as
/// plain text. Non-breaking spaces, because the bundled Geist has no wider or
/// thinner ones.
pub(super) const CHIP_ICON_SLOT: &str = "\u{00A0}\u{00A0}\u{00A0}\u{00A0}\u{00A0}";
/// Breathing room after a chip's label.
pub(super) const CHIP_TRAILING_PAD: &str = "\u{00A0}\u{00A0}";
/// The bundled face a chip's padding is shaped in. A space's advance follows
/// the font (Geist Mono's is 2.4× Geist's), and the counts above are tuned to
/// Geist, so pinning the padding keeps every chip's icon slot and insets the
/// same whatever the interface font. The label keeps the surrounding font.
pub(crate) const CHIP_PAD_FAMILY: &str = "Geist";
/// The icon sits at this size in its well.
const CHIP_ICON_SIZE: f32 = 14.0;

/// What a chip refers to; it picks the chip's icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Image,
    Skill,
    Command,
    File,
    Directory,
    /// Another chat, dropped from the sidebar.
    Chat,
    // slopty: words quoted from an answer, and a terminal's block or selection.
    /// Words quoted from the transcript; the agent receives them as a `>` quote.
    Quote,
    /// A terminal's block or selection; the agent receives it fenced.
    Context,
}

/// What a chip's icon is painted from: a monochrome glyph tinted like the
/// chip, or the file theme's own polychrome artwork, which tells formats and
/// languages apart.
pub(crate) enum ChipIcon {
    Glyph(&'static str),
    FileTheme(SharedString),
}

/// The padding around the label in a chip's display `range`: the side
/// bearing and icon slot before it, and the trailing room after it. Callers
/// shape both in [`CHIP_PAD_FAMILY`].
pub(crate) fn chip_pad_ranges(range: &Range<usize>) -> [Range<usize>; 2] {
    let lead_end = (range.start + MENTION_SIDE_PAD.len() + CHIP_ICON_SLOT.len()).min(range.end);
    let trail_start = range
        .end
        .saturating_sub(CHIP_TRAILING_PAD.len())
        .max(lead_end);
    [range.start..lead_end, trail_start..range.end]
}

/// How far to move a chip's pill down from the middle of its row, which
/// starts at `row_top`, so the pill centers its label's caps. GPUI puts a
/// line's baseline `(ascent - descent) / 2` below the row's middle, with the
/// tallest face's metrics, then rounds it to a device pixel (`scale` per
/// point). Geist's caps sit on the middle (1005 - 295 = 710, its cap height);
/// other faces and the rounding drift them by a pixel or so. Everything is in
/// window coordinates, since the rounding depends on where the row lands.
pub(super) fn label_center_offset(
    row_top: f32,
    line_height: f32,
    ascent: f32,
    descent: f32,
    cap_height: f32,
    scale: f32,
) -> f32 {
    if cap_height <= 0.0 {
        // A face without a cap height: keep the row's middle.
        return 0.0;
    }
    // GPUI's glyph snapping: halves round toward zero.
    let snap = |y: f32| {
        let device = y * scale;
        let rounded = if device.fract().abs() == 0.5 {
            device.trunc()
        } else {
            device.round()
        };
        rounded / scale
    };
    let baseline = snap(row_top + (line_height - ascent - descent) / 2.0 + ascent);
    let offset = baseline - cap_height / 2.0 - (row_top + line_height / 2.0);
    // The pill lands on whole device pixels too; a fraction would only
    // round it away from a label that is already centered.
    snap(offset)
}

/// How far to move a chip's pill down from the middle of its row so it
/// centers the label (see [`paint_chip`]). `label` is the label's first byte
/// within `line`, and `row_top` the row's top in window coordinates. The
/// shift stays within `room`, the pill's inset from its row, so it never
/// reaches into the next row.
pub(crate) fn chip_label_offset(
    line: &gpui::LineLayout,
    label: usize,
    row_top: Pixels,
    line_height: Pixels,
    room: Pixels,
    window: &Window,
) -> Pixels {
    // The face that actually shaped the label, fallbacks included.
    let Some(font_id) = line
        .runs
        .iter()
        .find(|run| run.glyphs.iter().any(|glyph| glyph.index >= label))
        .map(|run| run.font_id)
    else {
        return px(0.0);
    };
    let cap_height = window.text_system().cap_height(font_id, line.font_size);
    let offset = label_center_offset(
        f32::from(row_top),
        f32::from(line_height),
        f32::from(line.ascent),
        f32::from(line.descent),
        f32::from(cap_height),
        window.scale_factor(),
    );
    px(offset.clamp(-f32::from(room), f32::from(room)))
}

/// [`chip_label_offset`] for a chip at byte `label` of a text element's
/// `layout`, whose lines are its text split at newlines.
pub(crate) fn text_chip_label_offset(
    layout: &gpui::TextLayout,
    label: usize,
    row_top: Pixels,
    room: Pixels,
    window: &Window,
) -> Pixels {
    let mut start = 0;
    for line in layout.line_layouts() {
        let end = start + line.unwrapped_layout.len;
        if label < end {
            return chip_label_offset(
                &line.unwrapped_layout,
                label - start,
                row_top,
                layout.line_height(),
                room,
                window,
            );
        }
        start = end + 1;
    }
    px(0.0)
}

/// The non-empty padding ranges of `spans`, in order: what [`chip_text`]
/// shapes in [`CHIP_PAD_FAMILY`].
pub(super) fn chip_pad_overrides(spans: &[SentMentionSpan]) -> Vec<Range<usize>> {
    spans
        .iter()
        .flat_map(|span| chip_pad_ranges(&span.range))
        .filter(|range| !range.is_empty())
        .collect()
}

/// The icon of a chip. `path` names the file or folder for the file kinds.
pub(crate) fn chip_icon(
    kind: ChipKind,
    path: &str,
    appearance: crate::theme::Appearance,
) -> ChipIcon {
    use crate::file_icons::{FileIconIdentity, asset_path};
    match kind {
        ChipKind::Image => ChipIcon::Glyph(crate::icons::GALLERY),
        ChipKind::Skill => ChipIcon::Glyph(crate::icons::MAGIC_STICK_3),
        ChipKind::Command => ChipIcon::Glyph(crate::icons::COMMAND),
        ChipKind::Chat => ChipIcon::Glyph(crate::icons::CHAT_ROUND_LINE),
        ChipKind::Quote => ChipIcon::Glyph(crate::icons::DOCUMENT), // slopty: quotes.
        ChipKind::Context => ChipIcon::Glyph(crate::icons::TERMINAL), // slopty: terminal context.
        ChipKind::File => ChipIcon::FileTheme(asset_path(FileIconIdentity::file(path), appearance)),
        ChipKind::Directory => ChipIcon::FileTheme(asset_path(
            FileIconIdentity::directory(path.trim_end_matches('/'), false),
            appearance,
        )),
    }
}

/// Paint a chip like the transcript's file badges: a soft rounded pill whose
/// first row starts with the icon on a small well. Callers center `chip` on
/// the label's cap height: the row's middle moved by [`chip_label_offset`],
/// which is zero for Geist (ascent 1005, descent 295, cap 710 per 1000 units)
/// and corrects for other interface fonts.
pub(crate) fn paint_chip(
    window: &mut Window,
    chip: Bounds<Pixels>,
    icon: Option<&ChipIcon>,
    theme: &Theme,
    cx: &App,
) {
    window.paint_quad(quad(
        chip,
        px(5.0),
        theme.ink(0.09),
        px(0.0),
        gpui::transparent_black(),
        BorderStyle::default(),
    ));
    let Some(icon) = icon else {
        return;
    };
    let well = chip.size.height - px(2.0);
    let well_bounds = Bounds::new(chip.origin + point(px(1.0), px(1.0)), size(well, well));
    window.paint_quad(quad(
        well_bounds,
        px(4.0),
        crate::file_icons::well_bg(theme),
        px(0.0),
        gpui::transparent_black(),
        BorderStyle::default(),
    ));
    // The icon follows the chip's size, so chips on a small line stay tidy.
    let side = px(CHIP_ICON_SIZE).min(well - px(4.0));
    let bounds = Bounds::new(
        well_bounds.origin + point((well - side) / 2.0, (well - side) / 2.0),
        size(side, side),
    );
    match icon {
        ChipIcon::Glyph(path) => {
            let _ = window.paint_svg(
                bounds,
                (*path).into(),
                None,
                gpui::TransformationMatrix::default(),
                theme.text_muted,
                cx,
            );
        }
        ChipIcon::FileTheme(path) => {
            if let Some(image) = crate::file_icons::raster(path, cx) {
                // slopty: gpui-fast takes the image's bounds too; here they are the paint bounds.
                let _ = window.paint_image(bounds, bounds, Default::default(), image, 0, false);
            }
        }
    }
}

/// A sent message's text with its chips painted over their spans, for rows
/// that show one line of it (the queue). `text` is the projected display text
/// and `spans` its chips, as [`sent_mention_display`] returns them.
pub(crate) fn chip_text(
    text: SharedString,
    spans: Vec<SentMentionSpan>,
    theme: &Theme,
) -> gpui::AnyElement {
    if spans.is_empty() {
        return text.into_any_element();
    }
    // Split runs at the padding (a default highlight changes nothing else),
    // so the family override lands on whole runs over the inherited style.
    let pads = chip_pad_overrides(&spans);
    let styled = gpui::StyledText::new(text)
        .with_highlights(
            pads.iter()
                .map(|range| (range.clone(), gpui::HighlightStyle::default())),
        )
        .with_font_family_overrides(
            pads.into_iter()
                .map(|range| (range, SharedString::from(CHIP_PAD_FAMILY))),
        );
    let layout = styled.layout().clone();
    let icons: Vec<ChipIcon> = spans
        .iter()
        .map(|span| chip_icon(span.kind, &span.path, theme.appearance))
        .collect();
    let theme = theme.clone();
    let underlay = gpui::canvas(
        |_, _, _| (),
        move |_, _, window, cx| {
            for (span, icon) in spans.iter().zip(&icons) {
                let label = chip_pad_ranges(&span.range)[0].end;
                for (row, rect) in
                    crate::markdown::render::range_rects(&layout, &span.range, 0.0, 2.0)
                        .into_iter()
                        .enumerate()
                {
                    let row_top = rect.origin.y - px(2.0);
                    let offset = text_chip_label_offset(&layout, label, row_top, px(2.0), window);
                    let rect = Bounds::new(rect.origin + point(px(0.0), offset), rect.size);
                    paint_chip(window, rect, (row == 0).then_some(icon), &theme, cx);
                }
            }
        },
    )
    .absolute()
    .size_full();
    div()
        .relative()
        .child(underlay)
        .child(styled)
        .into_any_element()
}

// slopty: the quote and terminal-context chips. The draft holds each as a link whose
// destination carries the words, percent-encoded; the send turns it into what the agent reads
// (a `>` quote, a fenced block), so every agent takes it as plain Markdown.

/// The scheme of a quote chip's link.
pub(crate) const QUOTE_SCHEME: &str = "slopty-quote:";
/// The scheme of a terminal-context chip's link.
pub(crate) const CONTEXT_SCHEME: &str = "slopty-context:";

/// The longest label a quote chip shows, in characters.
const QUOTE_LABEL_CHARS: usize = 36;

fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = text.get(at + 1..at + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                at += 3;
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') => {
                out.push(byte);
                at += 1;
            }
            _ => return None,
        }
    }
    String::from_utf8(out).ok()
}

fn escape_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('`', "\\`")
}

/// What a quote chip reads: its first words on one line, in quotation marks.
pub(crate) fn quote_label(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut label = line.clone();
    if line.chars().count() > QUOTE_LABEL_CHARS {
        // The ellipsis is one of the label's characters.
        let cut: String = line.chars().take(QUOTE_LABEL_CHARS - 1).collect();
        label = cut.trim_end().to_owned() + "…";
    }
    format!("“{label}”")
}

/// A quote chip's link for `text`, or `None` for words that are only space.
pub(crate) fn quote_link(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| {
        format!("[{}]({QUOTE_SCHEME}{})", escape_label(&quote_label(text)), encode_component(text))
    })
}

/// A terminal-context chip's link: `fenced` is the block the terminal hands over (fenced and
/// named after its command), the chip "Terminal · 24 lines".
pub(crate) fn context_link(fenced: &str) -> Option<String> {
    let fenced = fenced.trim_end_matches('\n');
    let body = fenced_body(fenced)?;
    let lines = body.lines().count().max(1);
    let label = format!("Terminal · {lines} line{}", if lines == 1 { "" } else { "s" });
    Some(format!("[{}]({CONTEXT_SCHEME}{})", escape_label(&label), encode_component(fenced)))
}

/// The inside of a fenced block that is the whole of `text`.
fn fenced_body(text: &str) -> Option<&str> {
    let text = text.trim();
    let fence_len = text.bytes().take_while(|b| *b == b'`').count();
    if fence_len < 3 {
        return None;
    }
    let fence = &text[..fence_len];
    let (_, rest) = text.split_once('\n')?;
    let body = rest.strip_suffix(fence)?;
    Some(body.strip_suffix('\n').unwrap_or(body))
}

/// One quote or terminal-context chip in a draft.
pub(crate) struct ContextChipLink {
    pub(crate) range: Range<usize>,
    pub(crate) kind: ChipKind,
    pub(crate) label: String,
    pub(crate) text: String,
}

/// The quote and terminal-context chips in `raw`, in order: only canonical links, never
/// escaped text or code examples.
pub(crate) fn context_chip_links(raw: &str) -> Vec<ContextChipLink> {
    if !raw.contains(QUOTE_SCHEME) && !raw.contains(CONTEXT_SCHEME) {
        return Vec::new();
    }
    let mut links = Vec::new();
    let mut open: Option<(Range<usize>, ChipKind, String)> = None;
    let mut label = String::new();
    for (event, range) in pulldown_cmark::Parser::new(raw).into_offset_iter() {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => {
                let (kind, encoded) = if let Some(rest) = dest_url.strip_prefix(QUOTE_SCHEME) {
                    (ChipKind::Quote, rest)
                } else if let Some(rest) = dest_url.strip_prefix(CONTEXT_SCHEME) {
                    (ChipKind::Context, rest)
                } else {
                    continue;
                };
                if let Some(text) = decode_component(encoded) {
                    open = Some((range, kind, text));
                    label.clear();
                }
            }
            pulldown_cmark::Event::Text(text) if open.is_some() => label.push_str(&text),
            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Link) => {
                if let Some((range, kind, text)) = open.take() {
                    links.push(ContextChipLink { range, kind, label: label.clone(), text });
                }
            }
            _ => {}
        }
    }
    links
}

/// `raw` as the agent receives it: each quote chip a `>` quote on lines of its own, each
/// terminal-context chip its fenced block.
pub(crate) fn expand_context_chips(raw: &str) -> String {
    let links = context_chip_links(raw);
    if links.is_empty() {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut at = 0;
    for link in links {
        let before = &raw[at..link.range.start];
        out.push_str(before);
        let block = match link.kind {
            ChipKind::Quote => link
                .text
                .lines()
                .map(|line| if line.is_empty() { ">".to_owned() } else { format!("> {line}") })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => link.text.clone(),
        };
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&block);
        out.push('\n');
        // A blank line ends the quote or block before the person's own words, which would
        // otherwise run on into the quote.
        let rest = &raw[link.range.end..];
        let words = rest.trim_start_matches([' ', '\n']);
        if !words.is_empty() {
            out.push('\n');
        }
        at = raw.len() - words.len();
    }
    out.push_str(&raw[at..]);
    out
}

#[cfg(test)]
mod context_chip_tests {
    use super::*;

    #[test]
    fn a_quote_reaches_the_agent_as_a_quote() {
        let link = quote_link("The cache is\nstale [here]").unwrap();
        assert!(link.starts_with("[“The cache is stale \\[here\\]”](slopty-quote:"), "{link}");
        let draft = format!("{link} why?");
        let links = context_chip_links(&draft);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, ChipKind::Quote);
        assert_eq!(links[0].text, "The cache is\nstale [here]");
        assert_eq!(expand_context_chips(&draft), "> The cache is\n> stale [here]\n\nwhy?");
        assert_eq!(quote_link("  \n "), None);
    }

    #[test]
    fn a_terminal_block_reaches_the_agent_fenced() {
        let fenced = "```sh\n$ make\nerror: no rule\n```\n";
        let link = context_link(fenced).unwrap();
        assert!(link.starts_with("[Terminal · 2 lines](slopty-context:"), "{link}");
        let draft = format!("Why does this fail?\n{link}\n");
        assert_eq!(
            expand_context_chips(&draft),
            "Why does this fail?\n```sh\n$ make\nerror: no rule\n```\n"
        );
        assert!(context_link("plain words").is_none());
    }

    #[test]
    fn a_long_quote_label_is_cut_and_literal_links_stay() {
        let label = quote_label(&"word ".repeat(20));
        assert!(label.chars().count() <= 2 + QUOTE_LABEL_CHARS, "{label}");
        assert!(label.ends_with("…”") && !label.contains(" …"), "{label}");
        assert_eq!(quote_label("  short   words "), "“short words”");
        let literal = "`[x](slopty-quote:abc)`";
        assert!(context_chip_links(literal).is_empty());
        assert_eq!(expand_context_chips(literal), literal);
    }
}
