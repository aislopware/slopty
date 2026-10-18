// slopty: Slopty's own file (not in zeron).
//! slopty: what the agent's mods draw (`slopty_view_doc::ModSurface`), in the shell's own
//! components, no mod special: a band is a strip over the composer, a toast a toast
//! (`composer/dock.rs`), a status a footer segment (`composer/footer.rs`), a log line a quiet
//! caption, a pane a right-pane tab ([`ModPane`], opened through [`PaneAsk`]).
//!
//! A surface's element tree is nodes `{type, props, children}` of `Box`, `Text`, `Button`,
//! `Input`, `Select`, `Link`, `Code`, `Markdown` and `Svg`; a node of a type this build does not
//! know draws its words and its children, never nothing. What the person does in one (a press,
//! words entered, a pick, a close) goes back to the mod by `slopty.SurfaceEvent`, named by the
//! node's `id`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use slopty_view_doc::ModSurface;
use gpui::AnyElement;
use slopty_view_doc::calls;

use super::*;

/// Sends what the person did in a surface: (surface, element, event, value).
pub(crate) type Emit = Rc<dyn Fn(&str, &str, &str, Option<serde_json::Value>, &mut App)>;

/// The most nodes a surface draws, so a runaway tree cannot stall a frame.
const MAX_NODES: usize = 400;

/// A pane a mod opened, for the shell to show as a right-pane tab: the chat's mod surface
/// `surface`, titled `title`. The shell observes this global; `seq` grows with every ask.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneAsk {
    pub chat_id: String,
    pub surface_id: String,
    pub title: String,
    pub seq: u64,
}

impl gpui::Global for PaneAsk {}

/// What a surface's controls keep between frames.
#[derive(Default)]
pub(crate) struct SurfaceState {
    inputs: HashMap<String, (Entity<ComposerInput>, Subscription)>,
    svgs: HashMap<u64, Option<crate::image_media::MediaImage>>,
    /// Bands closed here while their mod takes no events.
    closed: HashSet<String>,
    /// Panes already asked for, by surface.
    panes: HashSet<String>,
}

fn prop<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    node.get("props").and_then(|props| props.get(key)).or_else(|| node.get(key))
}

fn prop_str<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    prop(node, key).and_then(serde_json::Value::as_str)
}

fn prop_bool(node: &serde_json::Value, key: &str) -> bool {
    prop(node, key).and_then(serde_json::Value::as_bool).unwrap_or(false)
}

fn prop_f32(node: &serde_json::Value, key: &str) -> Option<f32> {
    prop(node, key).and_then(serde_json::Value::as_f64).map(|n| n as f32)
}

/// A node's words: its `text`, `label` or `value`, else its string children joined.
pub(crate) fn node_words(node: &serde_json::Value) -> String {
    if let Some(text) = node.as_str() {
        return text.to_owned();
    }
    for key in ["text", "label", "value", "content"] {
        if let Some(text) = prop_str(node, key) {
            return text.to_owned();
        }
    }
    node.get("children")
        .and_then(serde_json::Value::as_array)
        .map(|children| children.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(""))
        .unwrap_or_default()
}

/// A node's type, lower-cased.
fn node_type(node: &serde_json::Value) -> String {
    node.get("type").and_then(serde_json::Value::as_str).unwrap_or("").to_ascii_lowercase()
}

/// The words a surface says for assistive technologies: every text in its tree, in order.
pub(crate) fn tree_words(node: &serde_json::Value, out: &mut Vec<String>) {
    match node {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Object(_) => {
            let words = node_words(node);
            let has_string_children = node
                .get("children")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|c| c.iter().any(serde_json::Value::is_string));
            if !words.is_empty() && !has_string_children {
                out.push(words);
            }
            if let Some(children) = node.get("children").and_then(serde_json::Value::as_array) {
                for child in children {
                    tree_words(child, out);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| tree_words(item, out)),
        _ => {}
    }
}

/// The options of a `Select`: (value, label).
fn select_options(node: &serde_json::Value) -> Vec<(String, String)> {
    prop(node, "options")
        .and_then(serde_json::Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| match option {
                    serde_json::Value::String(value) => Some((value.clone(), value.clone())),
                    serde_json::Value::Object(_) => {
                        let value = option.get("value").and_then(serde_json::Value::as_str)?;
                        let label = option.get("label").and_then(serde_json::Value::as_str).unwrap_or(value);
                        Some((value.to_owned(), label.to_owned()))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tone_color(tone: Option<&str>, theme: &Theme) -> gpui::Hsla {
    match tone {
        Some("muted" | "secondary") => theme.text_muted,
        Some("faint" | "dim") => theme.text_faint,
        Some("accent" | "info") => theme.accent,
        Some("success") => theme.success,
        Some("warning") => theme.warning,
        Some("error" | "danger") => theme.danger,
        _ => theme.text,
    }
}

impl SurfaceState {
    /// Draw `surface`'s tree. `emit` sends what the person does; `live`: the mod takes events.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_tree<V: 'static>(
        &mut self,
        surface: &ModSurface,
        live: bool,
        emit: &Emit,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyElement {
        let mut budget = MAX_NODES;
        if surface.tree.is_null() {
            let words = surface.text.clone().unwrap_or_default();
            return div()
                .text_size(crate::typography::ui_rems(12.5))
                .text_color(theme.text_muted)
                .child(SharedString::from(words))
                .into_any_element();
        }
        self.node(&surface.id, &surface.tree, "0".to_owned(), live, emit, theme, window, &mut budget, cx)
    }

    #[allow(clippy::too_many_arguments)]
    fn node<V: 'static>(
        &mut self,
        surface: &str,
        node: &serde_json::Value,
        path: String,
        live: bool,
        emit: &Emit,
        theme: &Theme,
        window: &mut Window,
        budget: &mut usize,
        cx: &mut Context<V>,
    ) -> AnyElement {
        if *budget == 0 {
            return gpui::Empty.into_any_element();
        }
        *budget -= 1;
        if let Some(text) = node.as_str() {
            return div().text_size(crate::typography::ui_rems(12.5)).text_color(theme.text).child(SharedString::from(text.to_owned())).into_any_element();
        }
        if let Some(items) = node.as_array() {
            let children: Vec<AnyElement> = items
                .iter()
                .enumerate()
                .map(|(ix, item)| self.node(surface, item, format!("{path}.{ix}"), live, emit, theme, window, budget, cx))
                .collect();
            return div().flex().flex_col().gap(px(6.0)).children(children).into_any_element();
        }
        let id = prop_str(node, "id").map(str::to_owned);
        let element_key = SharedString::from(format!("mod-{surface}-{}", id.clone().unwrap_or_else(|| path.clone())));
        let kind = node_type(node);
        let children = |this: &mut Self, window: &mut Window, budget: &mut usize, cx: &mut Context<V>| -> Vec<AnyElement> {
            node.get("children")
                .and_then(serde_json::Value::as_array)
                .map(|children| {
                    children
                        .iter()
                        .enumerate()
                        .map(|(ix, child)| this.node(surface, child, format!("{path}.{ix}"), live, emit, theme, window, budget, cx))
                        .collect()
                })
                .unwrap_or_default()
        };
        match kind.as_str() {
            "box" | "row" | "column" | "stack" | "group" => {
                let row = kind == "row" || prop_str(node, "direction").is_some_and(|d| d.starts_with("row") || d == "horizontal");
                let gap = prop_f32(node, "gap").unwrap_or(if row { 8.0 } else { 6.0 }).clamp(0.0, 32.0);
                let pad = prop_f32(node, "padding").unwrap_or(0.0).clamp(0.0, 32.0);
                let kids = children(self, window, budget, cx);
                div()
                    .flex()
                    .when(row, |el| el.flex_row().items_center().flex_wrap())
                    .when(!row, |el| el.flex_col())
                    .when(prop_str(node, "justify").is_some_and(|j| j.contains("between")), |el| el.justify_between())
                    .when(prop_bool(node, "border"), |el| {
                        el.border_1().border_color(crate::theme::hairline(0.08)).rounded(px(8.0))
                    })
                    .gap(px(gap))
                    .p(px(pad))
                    .children(kids)
                    .into_any_element()
            }
            "text" | "label" | "heading" => {
                let words = node_words(node);
                let heading = kind == "heading" || prop_str(node, "size") == Some("large");
                let color = tone_color(prop_str(node, "tone").or(prop_str(node, "color")), theme);
                let color = if prop_bool(node, "dim") { color.opacity(0.6) } else { color };
                div()
                    .text_size(crate::typography::ui_rems(if heading { 13.5 } else if prop_str(node, "size") == Some("small") { 11.5 } else { 12.5 }))
                    .line_height(crate::typography::ui_rems(if heading { 19.0 } else { 17.0 }))
                    .text_color(color)
                    .when(heading || prop_bool(node, "bold"), |el| el.font_weight(gpui::FontWeight::MEDIUM))
                    .when(prop_bool(node, "italic"), |el| el.italic())
                    .when(prop_bool(node, "mono"), |el| el.font_family(theme.font_mono.clone()))
                    .child(SharedString::from(words))
                    .into_any_element()
            }
            "button" => {
                let label = node_words(node);
                let disabled = prop_bool(node, "disabled") || !live || id.is_none();
                let variant = prop_str(node, "variant").unwrap_or("secondary");
                let base = match variant {
                    "primary" => crate::popover::btn_primary(theme, &label),
                    "danger" | "destructive" => crate::popover::btn_danger(theme, &label),
                    _ => crate::popover::btn_ghost(theme, &label, element_key.clone())
                        .border_1()
                        .border_color(crate::theme::hairline(0.08)),
                };
                let (emit, surface, element) = (emit.clone(), surface.to_owned(), id.clone().unwrap_or_default());
                base.id(element_key)
                    .role(Role::Button)
                    .aria_label(SharedString::from(label))
                    .flex_none()
                    .py(px(4.0))
                    .text_size(crate::typography::ui_rems(12.5))
                    // slopty: one dimming for what cannot act now.
                    .when(disabled, |el| el.opacity(crate::theme::DISABLED_OPACITY).cursor_default().aria_disabled(true))
                    .when(!disabled, |el| {
                        el.on_click(move |_, _, cx| emit(&surface, &element, "press", None, cx))
                    })
                    .into_any_element()
            }
            "input" | "textfield" | "textinput" => {
                let element = id.clone().unwrap_or_else(|| path.clone());
                let key = format!("{surface}/{element}");
                let placeholder = prop_str(node, "placeholder").unwrap_or("").to_owned();
                if !self.inputs.contains_key(&key) {
                    let initial = prop_str(node, "value").unwrap_or("").to_owned();
                    let input = cx.new(|cx| {
                        let mut input = ComposerInput::new(placeholder.clone(), cx).with_single_line();
                        input.set_text(initial, cx);
                        input
                    });
                    let (emit, surface_id, element_id) = (emit.clone(), surface.to_owned(), element.clone());
                    let subscription = cx.subscribe(&input, move |_, input, event: &ComposerInputEvent, cx| {
                        if *event == ComposerInputEvent::Submitted {
                            let value = input.read(cx).text().to_owned();
                            emit(&surface_id, &element_id, "input", Some(serde_json::Value::String(value)), cx);
                        }
                    });
                    self.inputs.insert(key.clone(), (input, subscription));
                }
                let input = self.inputs.get(&key).map(|(input, _)| input.clone());
                div()
                    .id(element_key)
                    .min_w(px(160.0))
                    .h(px(30.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(crate::theme::hairline(0.1))
                    .bg(crate::theme::ink(0.03))
                    .when(!live, |el| el.opacity(crate::theme::DISABLED_OPACITY))
                    .children(input)
                    .into_any_element()
            }
            "select" | "picker" | "radio" => {
                let current = prop_str(node, "value").map(str::to_owned);
                let disabled = !live || id.is_none();
                let options = select_options(node);
                let element = id.clone().unwrap_or_default();
                div()
                    .id(element_key.clone())
                    .role(Role::RadioGroup)
                    .aria_label(SharedString::from(prop_str(node, "label").unwrap_or("Choose").to_owned()))
                    .flex()
                    .flex_wrap()
                    .gap(px(4.0))
                    .children(options.into_iter().enumerate().map(|(ix, (value, label))| {
                        let selected = current.as_deref() == Some(value.as_str());
                        let (emit, surface, element) = (emit.clone(), surface.to_owned(), element.clone());
                        let option_key = SharedString::from(format!("{element_key}-{ix}"));
                        let fade = SharedString::from(format!("{option_key}-hover"));
                        div()
                            .id(option_key)
                            .role(Role::RadioButton)
                            .aria_label(SharedString::from(label.clone()))
                            .aria_selected(selected)
                            .h(px(26.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .rounded(px(Theme::CONTROL_RADIUS))
                            .text_size(crate::typography::ui_rems(12.0))
                            .border_1()
                            .border_color(if selected { crate::theme::ink(0.16) } else { crate::theme::hairline(0.07) })
                            .bg(if selected { crate::theme::ink(0.09) } else { crate::theme::ink(0.025) })
                            .text_color(if selected { theme.text } else { theme.text_muted })
                            .when(!disabled, |el| {
                                // slopty: the faded hover, at the row strength.
                                let rest = if selected { crate::theme::ink(0.09) } else { crate::theme::ink(0.025) };
                                let lit = if selected { crate::theme::ink(0.09) } else { crate::theme::ink(0.05) };
                                el.cursor_pointer()
                                    .bg(crate::motion::hover_blend(&fade, rest, lit))
                                    .on_hover(crate::motion::hover_listener(fade.clone()))
                                    .on_click(
                                    move |_, _, cx| {
                                        emit(&surface, &element, "select", Some(serde_json::Value::String(value.clone())), cx)
                                    },
                                )
                            })
                            .child(SharedString::from(label))
                    }))
                    .into_any_element()
            }
            "link" | "a" => {
                let label = node_words(node);
                let href = prop_str(node, "href").or(prop_str(node, "url")).map(str::to_owned);
                let (emit, surface, element) = (emit.clone(), surface.to_owned(), id.clone());
                div()
                    .id(element_key)
                    .role(Role::Link)
                    .aria_label(SharedString::from(label.clone()))
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.accent)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(move |_, _, cx| {
                        if let Some(href) = href.as_deref().filter(|h| h.starts_with("https://") || h.starts_with("http://")) {
                            cx.open_url(href);
                        }
                        if let Some(element) = element.as_deref().filter(|_| live) {
                            emit(&surface, element, "press", None, cx);
                        }
                    })
                    .child(SharedString::from(label))
                    .into_any_element()
            }
            "code" | "pre" => div()
                .p(px(8.0))
                .rounded(px(8.0))
                .bg(crate::theme::ink(0.04))
                .border_1()
                .border_color(crate::theme::hairline(0.06))
                .font_family(theme.font_mono.clone())
                .text_size(px(12.0))
                .line_height(px(17.0))
                .text_color(theme.text)
                .child(SharedString::from(node_words(node)))
                .into_any_element(),
            "markdown" | "md" => {
                let tree = slopty_markdown::parse_full(&node_words(node));
                let opts = crate::markdown::render::RenderOptions::settled(element_key, crate::clock::now(cx));
                crate::markdown::render::render_tree(&tree, &opts, theme, window, &|_| None)
            }
            "svg" | "image" => {
                let markup = prop_str(node, "svg").or(prop_str(node, "markup")).or(prop_str(node, "src")).unwrap_or("");
                let hash = {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    markup.hash(&mut hasher);
                    hasher.finish()
                };
                let image = self
                    .svgs
                    .entry(hash)
                    .or_insert_with(|| {
                        markup
                            .trim_start()
                            .starts_with('<')
                            .then(|| crate::image_media::decode_image("image/svg+xml", markup.as_bytes().to_vec()).ok())
                            .flatten()
                    })
                    .clone();
                let width = prop_f32(node, "width");
                let height = prop_f32(node, "height");
                match image {
                    Some(media) => {
                        let (w, h) = match (width, height) {
                            (Some(w), Some(h)) => (w, h),
                            (Some(w), None) => (w, w * media.height / media.width.max(1.0)),
                            (None, Some(h)) => (h * media.width / media.height.max(1.0), h),
                            (None, None) => (media.width.min(320.0), media.width.min(320.0) * media.height / media.width.max(1.0)),
                        };
                        div()
                            .id(element_key)
                            .role(Role::Image)
                            .aria_label(SharedString::from(prop_str(node, "alt").unwrap_or("Image").to_owned()))
                            .child(gpui::img(media.image.clone()).w(px(w.clamp(1.0, 640.0))).h(px(h.clamp(1.0, 640.0))))
                            .into_any_element()
                    }
                    None => div()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(prop_str(node, "alt").unwrap_or("Image unavailable").to_owned()))
                        .into_any_element(),
                }
            }
            _ => {
                // A kind this build does not know: its words, then its children.
                let words = node_words(node);
                let kids = children(self, window, budget, cx);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .when(!words.is_empty(), |el| {
                        el.child(div().text_size(crate::typography::ui_rems(12.5)).text_color(theme.text).child(SharedString::from(words)))
                    })
                    .children(kids)
                    .into_any_element()
            }
        }
    }
}

/// Where a surface shows: a band unless it says it is one of the others.
pub(crate) fn is_band(surface: &ModSurface) -> bool {
    !matches!(
        surface.kind.as_str(),
        ModSurface::TOAST | ModSurface::STATUS | ModSurface::PANE | ModSurface::LOG
    )
}

/// What sends a surface event for chat `chat` through `engine`.
pub(crate) fn emitter(engine: Option<crate::state::EngineHandle>, chat: String) -> Emit {
    Rc::new(move |surface: &str, element: &str, event: &str, value: Option<serde_json::Value>, cx: &mut App| {
        let Some(engine) = engine.clone() else { return };
        let mut params = serde_json::json!({
            "chatId": chat, "surfaceId": surface, "element": element, "event": event,
        });
        if let Some(value) = value {
            params["value"] = value;
        }
        cx.spawn(async move |_| {
            if let Err(err) = engine.client().call(calls::SURFACE_EVENT, params).await {
                tracing::warn!(error = %err, "a mod surface's event did not go");
            }
        })
        .detach();
    })
}

impl Composer {
    /// The bands the chat's mods draw over the composer; panes asked of the shell once.
    pub(super) fn render_mod_bands(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(chat) = self.state.read(cx).selected_chat.clone() else { return Vec::new() };
        let Some(dock) = self.dock.dock_of(&chat).cloned() else { return Vec::new() };
        let now = crate::clock::utc(cx).timestamp_millis();
        for pane in dock.surfaces.iter().filter(|s| s.kind == ModSurface::PANE) {
            if self.surfaces.panes.insert(format!("{chat}/{}", pane.id)) {
                let seq = cx.try_global::<PaneAsk>().map_or(1, |last| last.seq.wrapping_add(1));
                cx.set_global(PaneAsk {
                    chat_id: chat.clone(),
                    surface_id: pane.id.clone(),
                    title: pane.title.clone().unwrap_or_else(|| pane.plugin.clone()),
                    seq,
                });
            }
        }
        let emit = emitter(self.state.read(cx).engine().cloned(), chat.clone());
        let live = dock.can.surfaces;
        let mut bands = Vec::new();
        for surface in dock.surfaces.iter().filter(|s| is_band(s)) {
            if self.surfaces.closed.contains(&surface.id) || surface.until.is_some_and(|until| until <= now) {
                continue;
            }
            let body = self.surfaces.render_tree(surface, live, &emit, theme, window, cx);
            let mut words = Vec::new();
            tree_words(&surface.tree, &mut words);
            let title = surface.title.clone().unwrap_or_default();
            let label = [surface.plugin.clone(), title.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
            let (close_id, close_emit) = (surface.id.clone(), emit.clone());
            let card = crate::queue::queue_panel_surface(theme, false)
                .child(
                    div()
                        .h(px(28.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(crate::typography::ui_rems(11.5))
                        .child(div().flex_none().text_color(theme.text_faint).child(SharedString::from(surface.plugin.clone())))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("mod-band-close-{}", surface.id)))
                                .role(Role::Button)
                                .aria_label("Close")
                                .flex_none()
                                .size(px(22.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(5.0))
                                .cursor_pointer()
                                // slopty: the faded hover, at the control strength.
                                .bg(crate::motion::hover_blend(
                                    &format!("mod-band-close-{}", surface.id),
                                    gpui::transparent_black(),
                                    crate::theme::wash(0.11),
                                ))
                                .on_hover(crate::motion::hover_listener(format!("mod-band-close-{}", surface.id)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if live {
                                        close_emit(&close_id, "", "close", None, cx);
                                    }
                                    this.surfaces.closed.insert(close_id.clone());
                                    cx.notify();
                                }))
                                .child(crate::icons::icon(crate::icons::CLOSE).size(px(10.0)).text_color(theme.text_muted)),
                        ),
                )
                .child(div().px(px(8.0)).pb(px(8.0)).child(body));
            bands.push(
                div()
                    .id(SharedString::from(format!("mod-band-{}", surface.id)))
                    .role(Role::Group)
                    .aria_label(SharedString::from(label))
                    .aria_description(SharedString::from(words.join(" ")))
                    .mx(px(QUEUE_SIDE_INSET))
                    .child(crate::frost::frosted(crate::queue::PANEL_RADIUS, crate::frost::MENU_BLUR, card))
                    .into_any_element(),
            );
        }
        bands
    }
}

/// A mod's pane as a right-pane tab: the chat's surface `surface`, drawn from the chat's dock as
/// the composer last watched it, live while the mod keeps it.
pub struct ModPane {
    state: Entity<crate::state::AppState>,
    chat: String,
    surface: String,
    title: SharedString,
    inner: SurfaceState,
    _dock: Subscription,
    // slopty: the pane's scroll, its edge fades and floating rail (a settings page's shape).
    scroll: crate::settings::widgets::PageScroll,
}

impl crate::popover::ScrollRailHost for ModPane {
    fn rail_bar(&mut self) -> &mut crate::popover::MenuScrollbarState {
        crate::popover::ScrollRailHost::rail_bar(&mut self.scroll)
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        crate::popover::ScrollRailHost::rail_scroll(&self.scroll)
    }
}

impl ModPane {
    pub fn new(state: Entity<crate::state::AppState>, ask: &PaneAsk, cx: &mut Context<Self>) -> Self {
        Self {
            state,
            chat: ask.chat_id.clone(),
            surface: ask.surface_id.clone(),
            title: ask.title.clone().into(),
            inner: SurfaceState::default(),
            _dock: cx.observe_global::<super::dock::DockCache>(|_, cx| cx.notify()),
            scroll: crate::settings::widgets::PageScroll::default(),
        }
    }

    /// The tab's title.
    pub fn title(&self) -> SharedString {
        self.title.clone()
    }
}

impl Render for ModPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let dock = super::dock::chat_dock(&self.chat, cx);
        let surface = dock
            .as_ref()
            .and_then(|dock| dock.surfaces.iter().find(|s| s.id == self.surface && s.kind == ModSurface::PANE).cloned());
        let body = match surface {
            Some(surface) => {
                if let Some(title) = surface.title.clone() {
                    self.title = title.into();
                }
                let emit = emitter(self.state.read(cx).engine().cloned(), self.chat.clone());
                let live = dock.as_ref().is_some_and(|dock| dock.can.surfaces);
                self.inner.render_tree(&surface, live, &emit, &theme, window, cx)
            }
            None => div()
                .text_size(crate::typography::ui_rems(12.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(format!("{} closed this pane.", self.title)))
                .into_any_element(),
        };
        let page = div()
            .id("mod-pane")
            .role(Role::Group)
            .aria_label(self.title.clone())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll.scroll)
            .pt(px(Theme::TITLEBAR_HEIGHT + Theme::SPACE_LG))
            .px(px(Theme::SPACE_LG))
            .pb(px(Theme::SPACE_LG))
            .child(body);
        let rail = crate::popover::rail(self, "mod-pane-scrollbar", &theme, cx);
        div()
            .id("mod-pane-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.scroll.set_list_hovered(*hovered) {
                    cx.notify();
                }
            }))
            .child(crate::edge_fade::edge_faded(12.0, true, true, page).fade_overflow_y(&self.scroll.scroll))
            .children(rail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree's words read in order; an unknown kind still says its words.
    #[test]
    fn a_tree_says_its_words_in_order() {
        let tree = serde_json::json!({
            "type": "Box", "children": [
                {"type": "Text", "props": {"text": "Deploy"}},
                {"type": "Button", "props": {"id": "go", "label": "Ship it"}},
                {"type": "Gauge", "props": {"label": "72%"}},
                "plain words"
            ]
        });
        let mut words = Vec::new();
        tree_words(&tree, &mut words);
        assert_eq!(words, ["Deploy", "Ship it", "72%", "plain words"]);
        let select = serde_json::json!({"type": "Select", "props": {"options": ["a", {"value": "b", "label": "Bee"}]}});
        assert_eq!(select_options(&select), [("a".into(), "a".into()), ("b".into(), "Bee".into())]);
    }
}
