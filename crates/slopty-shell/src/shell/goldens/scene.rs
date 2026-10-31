//! What a frame paints, as text a snapshot holds and a test compares: a line per primitive with
//! its depth, place, clip and colours, and a line per run of glyphs; and what holds between a
//! state's frames in the two themes: the same layout, no colour only the other theme has, and no
//! text dark sets fainter than light does.
//!
//! The lines are GPUI's own (`Window::painted_primitives`: the retained check's text, which two
//! frames painting the same pixels share), shortened: colours as `#rrggbbaa`, bounds as
//! `x,y w×h`, what every primitive of a kind holds alike left out, and the glyphs of a line of
//! text as one run. Which glyph a run holds is the words' business and its raster the picture's.

#![expect(
    clippy::arithmetic_side_effects,
    reason = "offsets into a line of text and counts of its glyphs, far inside usize"
)]

use std::collections::{BTreeMap, HashMap};

use image::RgbaImage;

/// What `painted` (`Window::painted_primitives`) paints, a line per primitive and per run of
/// glyphs, sorted.
///
/// A line starts with its depth, how many primitives lie under it. A run of glyphs takes its
/// lowest glyph's: the glyphs of a word overlap their neighbours' raster boxes, so their own
/// depths step up and down along it.
pub(super) fn lines(painted: &[String]) -> Vec<String> {
    let mut glyphs: BTreeMap<String, Vec<(u32, [f32; 4])>> = BTreeMap::new();
    let mut lines = Vec::new();
    for line in painted {
        let (depth, primitive) = line.split_once(' ').unwrap_or(("0", line));
        let depth = depth.parse::<u32>().unwrap_or_default();
        let text = tidy(primitive);
        let glyph =
            primitive.starts_with("MonochromeSprite") || primitive.starts_with("SubpixelSprite");
        match glyph.then(|| placed(&text)).flatten() {
            Some((bounds, rest)) => glyphs.entry(rest).or_default().push((depth, bounds)),
            None => lines.push(format!("{depth} {text}")),
        }
    }
    for (rest, boxes) in glyphs {
        for (depth, bounds, count) in runs(boxes) {
            lines.push(format!("{depth} glyphs ×{count} {} {rest}", at(bounds)));
        }
    }
    lines.sort_unstable();
    lines
}

/// Where a line of [`lines`] paints, in points: a run's bounds, or a primitive's; `None` for a
/// line that names none (a path's).
pub(super) fn place(line: &str) -> Option<[f32; 4]> {
    let value = match line.split_once(" glyphs ×") {
        Some((_, run)) => {
            let mut parts = run.splitn(4, ' ');
            let _count = parts.next()?;
            format!("{} {}", parts.next()?, parts.next()?)
        }
        None => field(line, "bounds")?.to_owned(),
    };
    rect(&value)
}

/// A place as the lines say it, `x,y w×h`, in points.
fn rect(value: &str) -> Option<[f32; 4]> {
    let (place, size) = value.split_once(' ')?;
    let (x, y) = place.split_once(',')?;
    let (w, h) = size.split_once('×')?;
    Some([x.parse().ok()?, y.parse().ok()?, w.parse().ok()?, h.parse().ok()?])
}

/// The lines of a frame drawn in one theme that paint a colour only the other theme has, `own`
/// and `other` being the two themes' colours: a colour left as it was when the theme changed. A
/// colour is the other theme's when it is one of its tokens at that token's opacity: a colour both
/// themes have, or neither (one the shell works out itself, a white at a hover's opacity over a
/// dark ground), is the frame's own. Empty when every colour is.
pub(super) fn foreign(lines: &[String], own: &[[u8; 4]], other: &[[u8; 4]]) -> Vec<String> {
    let has = |theme: &[[u8; 4]], colour: [u8; 4]| {
        theme.iter().any(|token| token.iter().zip(colour).all(|(a, b)| a.abs_diff(b) <= 1))
    };
    lines
        .iter()
        .filter_map(|line| {
            let stray: Vec<[u8; 4]> = colours(line)
                .into_iter()
                .filter(|&colour| colour[3] > 0 && has(other, colour) && !has(own, colour))
                .collect();
            (!stray.is_empty()).then(|| format!("{} is the other theme's: {line}", hexes(&stray)))
        })
        .collect()
}

/// What one theme's frame of a state lays out that the other's does not, their colours and depths
/// aside: each primitive one frame paints that the other paints nowhere (the same kind, place,
/// clip, corners, borders and fade), and each run of glyphs the other has nowhere within a point
/// (a glyph's raster is a pixel wider or narrower on the other ground). Dark's lit edge
/// ([`lit_edge`]) is the theme's, not the layout's. Empty when the two lay out alike.
pub(super) fn moved(light: &[String], dark: &[String]) -> Vec<String> {
    let mut said = Vec::new();
    let (light_shapes, light_runs) = shapes(light);
    let (mut dark_shapes, dark_runs) = shapes(dark);
    for (shape, lines) in light_shapes {
        let theirs = dark_shapes.remove(&shape).unwrap_or_default();
        said.extend(lines.iter().skip(theirs.len()).map(|line| format!("only in light: {line}")));
        said.extend(theirs.iter().skip(lines.len()).map(|line| format!("only in dark: {line}")));
    }
    said.extend(dark_shapes.into_values().flatten().map(|line| format!("only in dark: {line}")));
    let mut unmatched: Vec<Option<&Run<'_>>> = dark_runs.iter().map(Some).collect();
    for run in &light_runs {
        match unmatched.iter_mut().find(|slot| slot.is_some_and(|other| other.near(run))) {
            Some(slot) => *slot = None,
            None => said.push(format!("only in light: {}", run.line)),
        }
    }
    said.extend(unmatched.into_iter().flatten().map(|run| format!("only in dark: {}", run.line)));
    said.retain(|line| !line.strip_prefix("only in dark: ").is_some_and(lit_edge));
    said
}

/// Whether `line` is the highlight a float's top inside edge takes on dark only
/// (`Theme::elevation`): an inset shadow with no blur, painted over the fill, taking no room.
fn lit_edge(line: &str) -> bool {
    line.split_once(' ').is_some_and(|(_, text)| text.starts_with("Shadow "))
        && field(line, "inset") == Some("1")
        && field(line, "blur_radius") == Some("0")
}

/// A run of glyphs of [`lines`]: what it says without its depth, colour and bounds, its bounds,
/// and its line.
struct Run<'a> {
    shape: String,
    bounds: [f32; 4],
    line: &'a str,
}

impl Run<'_> {
    /// Whether `other` is this run drawn in the other theme: the same glyphs, within a point.
    fn near(&self, other: &Run<'_>) -> bool {
        other.shape == self.shape
            && other.bounds.iter().zip(self.bounds).all(|(a, b)| (a - b).abs() <= 1.0)
    }

    /// How many primitives lie under it ([`lines`]).
    fn depth(&self) -> Option<u32> {
        depth(self.line)
    }
}

/// The depth a line of [`lines`] starts with.
fn depth(line: &str) -> Option<u32> {
    line.split_once(' ').and_then(|(depth, _)| depth.parse().ok())
}

/// `lines` without their depths and colours: the primitives by what is left of them, and the runs
/// of glyphs apart, their bounds left out of what they say.
fn shapes(lines: &[String]) -> (BTreeMap<String, Vec<&str>>, Vec<Run<'_>>) {
    let (mut primitives, mut runs) = (BTreeMap::<String, Vec<&str>>::new(), Vec::new());
    for line in lines.iter().map(String::as_str) {
        let (_, text) = line.split_once(' ').unwrap_or(("", line));
        let shape = uncoloured(text);
        match (shape.strip_prefix("glyphs ×"), place(line)) {
            (Some(run), Some(bounds)) => {
                // `×count x,y w×h rest`: the count kept, the bounds compared apart.
                let mut parts = run.splitn(4, ' ');
                let (count, rest) = (parts.next().unwrap_or_default(), parts.nth(2));
                let shape = format!("glyphs ×{count} {}", rest.unwrap_or_default());
                runs.push(Run { shape, bounds, line });
            }
            _ => primitives.entry(shape).or_default().push(line),
        }
    }
    (primitives, runs)
}

/// `text` with each `#rrggbbaa` said as `#`.
fn uncoloured(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('#') {
        out.push_str(before);
        out.push('#');
        rest = match after.get(..8).and_then(parse_hex) {
            Some(_) => after.get(8..).unwrap_or_default(),
            None => after,
        };
    }
    out.push_str(rest);
    out
}

/// A state's frame in one theme as [`fainter`] reads it: its [`lines`], its picture, and where
/// the picture's top left lies, in points (a pixel a point).
pub(super) struct Drawn<'a> {
    lines: &'a [String],
    image: &'a RgbaImage,
    origin: [f32; 2],
}

impl<'a> Drawn<'a> {
    /// The frame whose [`lines`] and picture these are, the picture's top left at `origin`.
    pub(super) const fn new(lines: &'a [String], image: &'a RgbaImage, origin: [f32; 2]) -> Self {
        Self { lines, image, origin }
    }
}

/// The runs of glyphs `dark` sets fainter than `floor` and than `light` sets the same run, each
/// with how it stands in both: a run reads at `floor` or more against its ground, or, where light
/// sets it fainter by design (a hint, a disabled row, a separator), at no less than light does.
/// Light's picture is the one reviewed and kept; dark is held to it. A run shows where no opaque
/// quad painted after it covers it ([`hidden`]: the page under a dialog, a row under a code).
/// Empty when dark's text all stands.
pub(super) fn fainter(light: &Drawn<'_>, dark: &Drawn<'_>, floor: f32) -> Vec<String> {
    let lit = standing(light);
    standing(dark)
        .into_iter()
        .filter_map(|(run, ratio, ground)| {
            let twin = lit.iter().find(|(other, ..)| other.near(&run)).map(|&(_, ratio, _)| ratio);
            let least = twin.map_or(floor, |twin| twin.min(floor));
            let light = twin.map_or_else(|| "none".to_owned(), |twin| format!("{twin:.2}"));
            (ratio < least).then(|| {
                format!("{ratio:.2} against {} (light {light}): {}", hex(ground), run.line)
            })
        })
        .collect()
}

/// Each run of glyphs `drawn` shows, with WCAG's contrast ratio against the ground under it (1
/// for none, 21 for black on white) and that ground: the run's colour laid over the ground at its
/// opacity, the ground being the colour most of the run's box shows, which the glyphs' own pixels
/// leave alone.
fn standing<'a>(drawn: &Drawn<'a>) -> Vec<(Run<'a>, f32, [u8; 4])> {
    let (_, runs) = shapes(drawn.lines);
    let [from_left, from_top] = drawn.origin;
    runs.into_iter()
        .filter(|run| !hidden(run, drawn.lines))
        .filter_map(|run| {
            let [left, top, width, height] = run.bounds;
            let colour = colours(run.line).pop()?;
            let ground = ground(drawn.image, [left - from_left, top - from_top, width, height])?;
            let ratio = ratio(over(colour, ground), ground);
            let [red, green, blue] = ground;
            Some((run, ratio, [red, green, blue, u8::MAX]))
        })
        .collect()
}

/// Whether an opaque quad painted after `run` covers its box within the quad's clip.
fn hidden(run: &Run<'_>, lines: &[String]) -> bool {
    let Some(under) = run.depth() else { return false };
    let covers = |at: Option<[f32; 4]>| {
        at.is_some_and(|[x, y, w, h]| {
            let [rx, ry, rw, rh] = run.bounds;
            x <= rx && y <= ry && rx + rw <= x + w && ry + rh <= y + h
        })
    };
    lines.iter().any(|line| {
        let opaque = field(line, "background")
            .and_then(|colour| colour.strip_prefix('#'))
            .and_then(parse_hex)
            .is_some_and(|[.., alpha]| alpha == u8::MAX);
        line.split_once(' ').is_some_and(|(_, text)| text.starts_with("Quad "))
            && depth(line).is_some_and(|over| over > under)
            && opaque
            && covers(place(line))
            && covers(field(line, "content_mask").and_then(rect))
    })
}

/// The colour most of `rect`'s pixels show in `image` (points, a pixel each), a tie going to the
/// higher colour so a run reads the same ground every time; `None` for a rect outside it.
fn ground(image: &RgbaImage, [x, y, w, h]: [f32; 4]) -> Option<[u8; 3]> {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped pixels")]
    let pixel = |v: f32, most: u32| (v.max(0.0) as u32).min(most);
    let (left, top) = (pixel(x.floor(), image.width()), pixel(y.floor(), image.height()));
    let right = pixel((x + w).ceil(), image.width());
    let bottom = pixel((y + h).ceil(), image.height());
    let mut seen: HashMap<[u8; 3], u32> = HashMap::new();
    for py in top..bottom {
        for px in left..right {
            let [red, green, blue, _] = image.get_pixel(px, py).0;
            *seen.entry([red, green, blue]).or_default() += 1;
        }
    }
    seen.into_iter().max_by_key(|&(colour, count)| (count, colour)).map(|(colour, _)| colour)
}

/// `colour` laid over the opaque `ground` at its own opacity.
fn over([r, g, b, a]: [u8; 4], ground: [u8; 3]) -> [f32; 3] {
    let alpha = f32::from(a) / 255.0;
    let mix = |c: u8, under: u8| f32::from(c).mul_add(alpha, f32::from(under) * (1.0 - alpha));
    [mix(r, ground[0]), mix(g, ground[1]), mix(b, ground[2])]
}

/// WCAG's contrast ratio between two colours, in 0 to 255 a channel.
fn ratio(ink: [f32; 3], ground: [u8; 3]) -> f32 {
    let light = |[r, g, b]: [f32; 3]| {
        let linear = |c: f32| {
            let c = c / 255.0;
            if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.0722_f32.mul_add(linear(b), 0.2126_f32.mul_add(linear(r), 0.7152 * linear(g)))
    };
    let (a, b) = (light(ink), light(ground.map(f32::from)));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The theme's colours in the order `Debug` prints them, as `#rrggbbaa` reads them: a light
/// theme's and a dark theme's paired by place give the tokens' pairs.
pub(super) fn theme_colours(theme_debug: &str) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    let mut rest = theme_debug;
    while let Some((_, after)) = rest.split_once("Hsla {") {
        let Some((fields, next)) = inside(after, '{', '}') else { break };
        if let Some(colour) = hsla(fields) {
            out.push(colour);
        }
        rest = next;
    }
    out
}

/// The colours a line paints, as `#rrggbbaa` reads them.
fn colours(line: &str) -> Vec<[u8; 4]> {
    line.split('#').skip(1).filter_map(|after| after.get(..8).and_then(parse_hex)).collect()
}

fn hexes(colours: &[[u8; 4]]) -> String {
    colours.iter().map(|c| hex(*c)).collect::<Vec<_>>().join(",")
}

fn hex([r, g, b, a]: [u8; 4]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
}

fn parse_hex(hex: &str) -> Option<[u8; 4]> {
    if hex.len() != 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?])
}

/// `h: …, s: …, l: …, a: …` as the colour GPUI draws.
fn hsla(fields: &str) -> Option<[u8; 4]> {
    let mut values = [0.0_f32; 4];
    for (slot, name) in values.iter_mut().zip(["h:", "s:", "l:", "a:"]) {
        let (_, after) = fields.split_once(name)?;
        *slot = after.trim_start().split([',', ' ', '}']).next()?.parse().ok()?;
    }
    let [hue, saturation, lightness, alpha] = values;
    let rgba = gpui::hsla(hue, saturation, lightness, alpha).to_rgb();
    let byte = |v: f32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to a byte"
        )]
        let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        b
    };
    Some([byte(rgba.r), byte(rgba.g), byte(rgba.b), byte(rgba.a)])
}

/// `text`, which starts just past an `open`, split at the `close` that ends it: what it holds,
/// and what follows that close.
fn inside(text: &str, open: char, close: char) -> Option<(&str, &str)> {
    let mut depth = 0_usize;
    for (at, c) in text.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                let (held, after) = text.split_at_checked(at)?;
                return Some((held, after.strip_prefix(close)?));
            }
            depth -= 1;
        }
    }
    None
}

/// Each `{name}{open}…{close}` in `text` replaced by what `with` makes of what it holds, where
/// `with` gives something; innermost first, since `with` sees what it holds already rewritten.
fn rewrite(
    text: &str,
    name: &str,
    open: char,
    close: char,
    with: &dyn Fn(&str) -> Option<String>,
) -> String {
    let head = format!("{name}{open}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once(&head) {
        out.push_str(before);
        let Some((held, next)) = inside(after, open, close) else {
            out.push_str(&head);
            out.push_str(after);
            return out;
        };
        let inner = rewrite(held, name, open, close, with);
        if let Some(made) = with(inner.trim()) {
            out.push_str(&made);
        } else {
            out.push_str(&head);
            out.push_str(&inner);
            out.push(close);
        }
        rest = next;
    }
    out.push_str(rest);
    out
}

/// A number as the lines print it: whole when it is, else to two places.
fn number(text: &str) -> Option<String> {
    let value: f64 = text.trim().parse().ok()?;
    // Plus zero, so no line reads `-0`.
    let rounded = (value * 100.0).round() / 100.0 + 0.0;
    Some(format!("{rounded}"))
}

/// The value after `name: ` in `fields` ([`leading_value`]).
fn field<'a>(fields: &'a str, name: &str) -> Option<&'a str> {
    fields.split_once(&format!("{name}: ")).map(|(_, after)| leading_value(after))
}

/// The value `text` starts with, up to the next `, ` at its own depth (a place's own comma,
/// `x,y`, has no space after it) or the bracket that closes what holds it.
fn leading_value(text: &str) -> &str {
    let mut depth = 0_usize;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let ends = match c {
            '{' | '(' | '[' => {
                depth += 1;
                false
            }
            '}' | ')' | ']' => match depth.checked_sub(1) {
                Some(outer) => {
                    depth = outer;
                    false
                }
                None => true,
            },
            ',' => depth == 0 && chars.peek().is_some_and(|&(_, next)| next == ' '),
            _ => false,
        };
        if ends {
            return text.get(..at).map_or(text, str::trim);
        }
    }
    text.trim()
}

/// A primitive's `Debug` text, shortened as the module says.
fn tidy(primitive: &str) -> String {
    let mut text = rewrite(primitive, "Hsla ", '{', '}', &|fields| hsla(fields).map(hex));
    // What every sprite holds alike: its atlas tile (neutral in these lines) and, unturned, its
    // transform.
    text = rewrite(&text, "tile: AtlasTile ", '{', '}', &|_| Some(String::new()));
    text = text.replace(", , ", ", ");
    text = text.replace(
        ", transformation: TransformationMatrix { rotation_scale: [[1.0, 0.0], [0.0, 1.0]], translation: [0.0, 0.0] }",
        "",
    );
    text = text.replace("order: 0, ", "").replace("pad: 0, ", "");
    text = lengths(&text);
    text = rewrite(&text, "Point ", '{', '}', &|fields| {
        Some(format!("{},{}", field(fields, "x")?, field(fields, "y")?))
    });
    text = rewrite(&text, "Size ", '{', '}', &|held| {
        held.split_once(" × ").map(|(w, h)| format!("{}×{}", w.trim(), h.trim()))
    });
    text = rewrite(&text, "Bounds ", '{', '}', &|fields| {
        Some(format!("{} {}", field(fields, "origin")?, field(fields, "size")?))
    });
    text = rewrite(&text, "ContentMask ", '{', '}', &|fields| {
        field(fields, "bounds").map(str::to_owned)
    });
    text = rewrite(&text, "Corners ", '{', '}', &|fields| {
        alike(
            ["top_left", "top_right", "bottom_right", "bottom_left"]
                .map(|name| field(fields, name)),
        )
    });
    text = rewrite(&text, "Edges ", '{', '}', &|fields| {
        alike(["top", "right", "bottom", "left"].map(|name| field(fields, name)))
    });
    text = rewrite(&text, "Solid", '(', ')', &|colour| Some(colour.to_owned()));
    text = rewrite(&text, "LinearColorStop ", '{', '}', &|fields| {
        Some(format!("{}@{}", field(fields, "color")?, field(fields, "percentage")?))
    });
    // A fade's ramps, each as one number where its four edges agree, and those all zero left out.
    text = rewrite(&text, "EdgeFadeRamps ", '{', '}', &|fields| {
        let set: Vec<String> = entries(fields)
            .into_iter()
            .filter_map(|(name, value)| {
                let edges: Vec<&str> =
                    value.trim_matches(['[', ']']).split(", ").map(str::trim).collect();
                let zero = |edge: &&str| edge.parse::<f32>().is_ok_and(|n| n.abs() < f32::EPSILON);
                if edges.iter().all(zero) {
                    return None;
                }
                let first = edges.first().copied().unwrap_or_default();
                let said = if edges.iter().all(|edge| *edge == first) { first } else { value };
                Some(format!("{name}: {said}"))
            })
            .collect();
        Some(format!("{{ {} }}", set.join(", ")))
    });
    if (text.contains("border_widths: 0 ") || text.ends_with("border_widths: 0 }"))
        && let Some(colour) = field(&text, "border_color").map(str::to_owned)
    {
        text = text.replace(&format!("border_color: {colour}, "), "");
    }
    if text.starts_with("path ")
        && let Some((head, points)) = text.split_once(" [(")
    {
        let vertices = points.matches("), (").count() + 1;
        text = format!("{head} ×{vertices} vertices");
    }
    text
}

/// Four edges' or corners' values as one where they agree, else each in order.
fn alike(all: [Option<&str>; 4]) -> Option<String> {
    let [first, ..] = all;
    let first = first?;
    Some(if all.iter().all(|value| *value == Some(first)) {
        first.to_owned()
    } else {
        all.iter().map(|value| value.unwrap_or("?")).collect::<Vec<_>>().join("/")
    })
}

/// The `name: value` entries of a struct's fields, in order.
fn entries(fields: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = fields;
    while let Some((name, after)) = rest.split_once(": ") {
        let value = leading_value(after);
        out.push((name.trim_start_matches([',', ' ']), value));
        let Some(next) = after.trim_start().strip_prefix(value) else { break };
        rest = next;
    }
    out
}

/// Each length GPUI prints with its unit (`12.5px (scaled)`, `3 px (device)`, `4px`) as its
/// number alone, to two places.
fn lengths(text: &str) -> String {
    let united = text.replace("px (scaled)", "px").replace(" px (device)", "px");
    let mut out = String::with_capacity(united.len());
    let mut pieces = united.split("px").peekable();
    while let Some(piece) = pieces.next() {
        if pieces.peek().is_none() {
            out.push_str(piece);
            break;
        }
        let start = piece
            .char_indices()
            .rev()
            .take_while(|&(_, c)| c.is_ascii_digit() || c == '.' || c == '-')
            .last()
            .map_or(piece.len(), |(at, _)| at);
        let split = piece.split_at_checked(start);
        if let Some((before, Some(length))) = split.map(|(before, length)| (before, number(length)))
        {
            out.push_str(before);
            out.push_str(&length);
        } else {
            out.push_str(piece);
            out.push_str("px");
        }
    }
    out
}

/// A glyph line's bounds and the rest of it without them.
fn placed(text: &str) -> Option<([f32; 4], String)> {
    let (before, after) = text.split_once("bounds: ")?;
    let bounds = leading_value(after);
    let (place, size) = bounds.split_once(' ')?;
    let (x, y) = place.split_once(',')?;
    let (w, h) = size.split_once('×')?;
    let parsed = [x.parse().ok()?, y.parse().ok()?, w.parse().ok()?, h.parse().ok()?];
    let tail = after.strip_prefix(bounds)?;
    let tail = tail.strip_prefix(", ").unwrap_or(tail);
    Some((parsed, format!("{before}{tail}")))
}

/// The glyph boxes `boxes` as runs: each glyph joins the run whose last glyph it continues on a
/// line (overlapping it by half its height, at most a space to its right), else starts one.
/// Each run's bounds and how many glyphs it holds.
fn runs(mut boxes: Vec<(u32, [f32; 4])>) -> Vec<(u32, [f32; 4], usize)> {
    boxes.sort_by(|(_, a), (_, b)| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    let mut runs: Vec<(u32, [f32; 4], [f32; 4], usize)> = Vec::new();
    for (depth, b) in boxes {
        let joins = runs.iter_mut().find(|(_, _, last, _)| {
            let overlap = (last[1] + last[3]).min(b[1] + b[3]) - last[1].max(b[1]);
            let gap = b[0] - (last[0] + last[2]);
            overlap >= 0.5 * last[3].min(b[3]) && gap <= 0.6 * last[3].max(b[3]) && gap >= -last[2]
        });
        match joins {
            Some((lowest, union, last, count)) => {
                *lowest = (*lowest).min(depth);
                let right = (union[0] + union[2]).max(b[0] + b[2]);
                let bottom = (union[1] + union[3]).max(b[1] + b[3]);
                union[0] = union[0].min(b[0]);
                union[1] = union[1].min(b[1]);
                union[2] = right - union[0];
                union[3] = bottom - union[1];
                *last = b;
                *count += 1;
            }
            None => runs.push((depth, b, b, 1)),
        }
    }
    runs.into_iter().map(|(depth, union, _, count)| (depth, union, count)).collect()
}

fn at([x, y, w, h]: [f32; 4]) -> String {
    let said = |v: f32| number(&v.to_string()).unwrap_or_default();
    format!("{},{} {}×{}", said(x), said(y), said(w), said(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quad as `Window::painted_primitives` prints it.
    const QUAD: &str = "Quad { order: 0, border_style: Solid, bounds: Bounds { origin: Point { x: 448px (scaled), y: 160.5px (scaled) }, size: Size { 680px (scaled) × 60px (scaled) } }, content_mask: ContentMask { bounds: Bounds { origin: Point { x: 0px (scaled), y: 0px (scaled) }, size: Size { 1320px (scaled) × 880px (scaled) } } }, background: Solid(Hsla { h: 0.0, s: 0.0, l: 1.0, a: 1.0 }), border_color: Hsla { h: 0.0, s: 0.0, l: 0.0, a: 0.0 }, corner_radii: Corners { top_left: 10px (scaled), top_right: 10px (scaled), bottom_right: 10px (scaled), bottom_left: 10px (scaled) }, border_widths: Edges { top: 0px (scaled), right: 0px (scaled), bottom: 0px (scaled), left: 0px (scaled) } }";

    /// A glyph as `Window::painted_primitives` prints it, at `x`, `y`, `w` × `h`, grey at
    /// `lightness`.
    fn glyph(x: f32, y: f32, w: f32, h: f32, lightness: f32) -> String {
        format!(
            "3 MonochromeSprite {{ order: 0, pad: 0, tile: AtlasTile {{ texture_id: AtlasTextureId {{ index: 0, kind: Monochrome }}, tile_id: TileId(0), padding: 0, bounds: Bounds {{ origin: Point {{ x: 0 px (device), y: 0 px (device) }}, size: Size {{ 0 px (device) × 0 px (device) }} }} }}, bounds: Bounds {{ origin: Point {{ x: {x}px (scaled), y: {y}px (scaled) }}, size: Size {{ {w}px (scaled) × {h}px (scaled) }} }}, content_mask: ContentMask {{ bounds: Bounds {{ origin: Point {{ x: 0px (scaled), y: 0px (scaled) }}, size: Size {{ 1320px (scaled) × 880px (scaled) }} }} }}, color: Hsla {{ h: 0.0, s: 0.0, l: {lightness}, a: 1.0 }}, transformation: TransformationMatrix {{ rotation_scale: [[1.0, 0.0], [0.0, 1.0]], translation: [0.0, 0.0] }} }}"
        )
    }

    /// A quad reads as its place, clip, ground and corners; a border of no width says no colour.
    #[test]
    fn a_quad_reads_as_its_place_and_colours() {
        assert_eq!(
            lines(&[format!("2 {QUAD}")]),
            [
                "2 Quad { border_style: Solid, bounds: 448,160.5 680×60, content_mask: 0,0 1320×880, background: #ffffffff, corner_radii: 10, border_widths: 0 }"
            ],
            "the quad's line"
        );
    }

    /// A fade says the ramps it sets, each edge's once where they agree.
    #[test]
    fn a_fade_says_what_it_sets() {
        let faded = format!(
            "1 {QUAD} fade EdgeFadeRamps {{ edge: [0.0, 0.0, 0.0, 0.0], start: [1.0, 1.0, 1.0, 1.0], clip: [8.0, 4.0, 20.0, 40.0], radii: [12.0, 12.0, 12.0, 12.0] }}"
        );
        let [line] = &*lines(&[faded]) else { panic!("one line") };
        assert!(
            line.ends_with(" fade { start: 1.0, clip: [8.0, 4.0, 20.0, 40.0], radii: 12.0 }"),
            "{line}"
        );
    }

    /// The glyphs of a word and the next on its line are one run; a line under them, or another
    /// colour, another.
    #[test]
    fn a_line_of_glyphs_is_one_run() {
        let painted = [
            glyph(10.0, 10.0, 6.0, 9.0, 0.1),
            glyph(17.0, 8.0, 6.0, 11.0, 0.1),
            glyph(27.0, 10.0, 6.0, 9.0, 0.1),
            glyph(10.0, 30.0, 6.0, 9.0, 0.1),
            glyph(40.0, 10.0, 6.0, 9.0, 0.5),
        ];
        let lines = lines(&painted);
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[0].starts_with("3 glyphs ×1 10,30 6×9 MonochromeSprite"), "{lines:#?}");
        assert!(lines[1].starts_with("3 glyphs ×1 40,10 6×9"), "{lines:#?}");
        assert!(lines[2].starts_with("3 glyphs ×3 10,8 23×11"), "{lines:#?}");
        assert!(!lines[2].contains("tile") && !lines[2].contains("transformation"), "{lines:#?}");
    }

    /// A frame that paints a colour only the other theme has, at that colour's opacity, is named;
    /// a colour both themes have, or neither, or one with no opacity, or the other's at an
    /// opacity of the frame's own (a white hover over a dark ground), is the frame's own.
    #[test]
    fn a_frame_paints_no_colour_only_the_other_theme_has() {
        let light = [[255, 255, 255, 255], [20, 20, 22, 255], [217, 119, 87, 255]];
        let dark = [[30, 30, 32, 255], [232, 232, 234, 140], [217, 119, 87, 255]];
        let lines = [
            "1 Quad { background: #1e1e20ff }".to_owned(),
            "2 glyphs ×4 10,8 23×11 MonochromeSprite { color: #e8e8ea8c }".to_owned(),
            "2 Quad { background: #d97757ff, border_color: #ffffff00 }".to_owned(),
            "3 Quad { background: #4a90e2ff }".to_owned(),
            "4 Quad { background: #ffffff0f }".to_owned(),
        ];
        assert!(foreign(&lines, &dark, &light).is_empty(), "every colour the frame's own");
        let left = ["1 Quad { background: #ffffffff, border_color: #1e1e20ff }".to_owned()];
        let said = foreign(&left, &dark, &light);
        assert_eq!(said.len(), 1, "a light surface left in dark: {said:?}");
        assert!(said[0].starts_with("#ffffffff is the other theme's"), "{said:?}");
    }

    /// Two themes' frames lay out alike when only their colours and depths differ and a run of
    /// glyphs is a point off; a primitive moved, gone or new, or a run moved further, is said.
    #[test]
    fn a_layout_that_parts_between_the_themes_is_said() {
        let light = [
            "2 Quad { bounds: 0,0 10×10, background: #ffffffff }".to_owned(),
            "3 glyphs ×4 10,8 23×11 MonochromeSprite { color: #101012ff }".to_owned(),
        ];
        let dark = [
            "5 Quad { bounds: 0,0 10×10, background: #1e1e20ff }".to_owned(),
            "4 glyphs ×4 11,8 22×11 MonochromeSprite { color: #e8e8eaff }".to_owned(),
            "6 Shadow { blur_radius: 0, bounds: 0,0.5 10×10, color: #ffffff0f, inset: 1 }"
                .to_owned(),
        ];
        assert!(
            moved(&light, &dark).is_empty(),
            "dark's lit edge aside: {:?}",
            moved(&light, &dark)
        );
        let lower = [
            "5 Quad { bounds: 0,6 10×10, background: #1e1e20ff }".to_owned(),
            "4 glyphs ×4 10,10 23×11 MonochromeSprite { color: #e8e8eaff }".to_owned(),
            "1 Quad { bounds: 0,0 4×4, background: #00000000 }".to_owned(),
        ];
        let said = moved(&light, &lower);
        assert_eq!(said.len(), 5, "{said:#?}");
        assert!(said.iter().any(|line| line.starts_with("only in dark: 1 Quad")), "{said:#?}");
    }

    /// Dark's text is held against the ground most of its box shows, at its own opacity: at 3 or
    /// more, or no fainter than light sets it. A run under an opaque quad painted after it shows
    /// nowhere and is not held.
    #[test]
    fn dark_sets_no_text_fainter_than_light() {
        fn drawn<'a>(lines: &'a [String], image: &'a RgbaImage) -> Drawn<'a> {
            Drawn::new(lines, image, [100.0, 100.0])
        }
        let white = RgbaImage::from_pixel(40, 20, image::Rgba([255, 255, 255, 255]));
        let black = RgbaImage::from_pixel(40, 20, image::Rgba([20, 20, 22, 255]));
        let run = |colour: &str| {
            vec![format!("3 glyphs ×4 110,105 23×11 MonochromeSprite {{ color: #{colour} }}")]
        };
        let (ink, chalk) = (run("141416ff"), run("e8e8eaff"));
        let stands = fainter(&drawn(&ink, &white), &drawn(&chalk, &black), 3.0);
        assert!(stands.is_empty(), "light ink and dark chalk both stand: {stands:?}");
        let near = run("303035ff");
        let [said] = &*fainter(&drawn(&ink, &white), &drawn(&near, &black), 3.0) else {
            panic!("a literal near the dark ground is said")
        };
        assert!(said.starts_with("1.") && said.contains("against #141416ff (light 1"), "{said}");
        let (hint, dim) = (run("1414161a"), run("e8e8ea20"));
        assert!(
            fainter(&drawn(&hint, &white), &drawn(&dim, &black), 3.0).is_empty(),
            "a hint light sets at under 2 may be as faint in dark"
        );
        let mut covered = near;
        covered.push(
            "5 Quad { bounds: 100,100 40×20, content_mask: 0,0 1320×880, background: #141416ff }"
                .to_owned(),
        );
        let beneath = fainter(&drawn(&ink, &white), &drawn(&covered, &black), 3.0);
        assert!(beneath.is_empty(), "a run under an opaque quad shows nowhere: {beneath:?}");
    }

    /// The glyphs of a word are one run whatever depths their overlapping boxes give them, at the
    /// lowest of those.
    #[test]
    fn a_word_is_one_run_across_its_glyphs_depths() {
        let painted = [glyph(10.0, 10.0, 6.0, 9.0, 0.1), glyph(15.0, 10.0, 6.0, 9.0, 0.1)];
        let painted = [painted[0].replacen("3 ", "4 ", 1), painted[1].clone()];
        assert_eq!(lines(&painted).len(), 1, "{:#?}", lines(&painted));
        assert!(lines(&painted)[0].starts_with("3 glyphs ×2 10,10 11×9"), "{:#?}", lines(&painted));
    }

    /// A run and a primitive say where they paint; a path does not.
    #[test]
    fn a_line_says_where_it_paints() {
        let run = "3 glyphs ×4 10,8 23×11 MonochromeSprite { color: #000000ff }";
        assert_eq!(place(run), Some([10.0, 8.0, 23.0, 11.0]), "the run's bounds");
        let quad = "2 Quad { bounds: 448,160.5 680×60, content_mask: 0,0 1320×880 }";
        assert_eq!(place(quad), Some([448.0, 160.5, 680.0, 60.0]), "the quad's, not its clip");
        let path = "1 path 0,0 4×4 0,0 1320×880 #000000ff ×12 vertices";
        assert_eq!(place(path), None, "a path names no bounds");
    }

    /// The theme's colours come out of its `Debug` in order.
    #[test]
    fn a_themes_colours_are_read_in_order() {
        let debug = "Theme { bg: Hsla { h: 0.0, s: 0.0, l: 1.0, a: 1.0 }, x: Foo { y: Hsla { h: 0.0, s: 0.0, l: 0.0, a: 0.5 } } }";
        assert_eq!(theme_colours(debug), [[255, 255, 255, 255], [0, 0, 0, 128]], "both, in order");
    }
}
