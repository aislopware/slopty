// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
use super::*;

impl Shell {
    /// The entire conversation receives attachments in its own composer,
    /// regardless of which pane had keyboard focus when the drag started.
    pub(super) fn chat_dropzone(
        &self,
        id: impl Into<gpui::ElementId>,
        composer: Entity<Composer>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let images = composer.clone();
        let workspace = composer.clone();
        let mention = composer.clone();
        div()
            .id(id)
            .relative()
            .on_drop(cx.listener(move |_, paths: &gpui::ExternalPaths, _, cx| {
                images.update(cx, |composer, cx| {
                    composer.add_paths(paths.paths().to_vec(), cx)
                });
                cx.notify();
            }))
            .on_drop::<WorkspacePathDrag>(cx.listener(
                move |this, payload: &WorkspacePathDrag, window, cx| {
                    this.attach_workspace_drag(payload, &workspace, window, cx);
                    cx.notify();
                },
            ))
            // A session dragged from the sidebar becomes a chat chip here,
            // and its row slides home.
            .on_drop::<SidebarSessionDrag>(cx.listener(
                move |this, payload: &SidebarSessionDrag, window, cx| {
                    let title = this
                        .state
                        .read(cx)
                        .chats
                        .iter()
                        .find(|chat| chat.id == payload.chat_id)
                        .map(|chat| chat.title.clone().unwrap_or_default());
                    if let Some(title) = title {
                        mention.update(cx, |composer, cx| {
                            composer.add_chat_mention(&payload.chat_id, &title, window, cx)
                        });
                    }
                    this.cancel_sidebar_session_transfer(cx);
                    cx.stop_propagation();
                    cx.notify();
                },
            ))
            .on_drop::<RightTabDrag>(cx.listener(
                move |this, payload: &RightTabDrag, window, cx| {
                    if let Some(path) = &payload.workspace_path {
                        this.attach_workspace_drag(path, &composer, window, cx);
                    }
                    cx.notify();
                },
            ))
    }

    // slopty: drawn only while something is carried. Its hitbox lies over the whole chat, so
    // at rest it kept every live page under it from the pointer: a native takes the pointer only
    // where no later hitbox lies (`gpui::composition::NativeHitMap`), and no finger or click
    // reached a page (e2e `pages`). A drag draws every frame afresh, so it shows on the first.
    /// The target over chat `key` while a file is carried over it (opencode's dropzone): a veil
    /// that fades in, a drifting field of faint stripes held to the reading column, and a round
    /// badge with its words rising into place.
    pub(super) fn attachment_drop_overlay(
        key: &'static str,
        theme: &Theme,
        cx: &App,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        // Use typed drag styles rather than cached shell state: an external
        // FileDrop::Exited clears the payload without a final mouse move.
        // Resize markers and non-file tabs must never reveal the overlay.
        if !cx.has_active_drag() {
            return None;
        }
        let shown = drop_target_shown(key, cx);
        let over = drop_target_over(key);
        Some(
            drop_target(key, crate::icons::PAPERCLIP, "Drop to attach", shown, theme)
                .drag_over::<gpui::ExternalPaths>(move |style, _, window, _| over(style, window))
                .drag_over::<WorkspacePathDrag>(move |style, _, window, _| over(style, window))
                .drag_over::<RightTabDrag>(move |style, tab, window, _| {
                    if tab.workspace_path.is_some() { over(style, window) } else { style }
                }),
        )
    }

    /// The hint while a sidebar session is carried over a chat.
    // slopty: drawn only while something is carried, as the attachment's overlay is.
    pub(super) fn mention_drop_overlay(
        key: &'static str,
        theme: &Theme,
        cx: &App,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if !cx.has_active_drag() {
            return None;
        }
        let key = mention_key(key);
        let shown = drop_target_shown(key, cx);
        let over = drop_target_over(key);
        Some(
            drop_target(key, crate::icons::CHAT_ROUND_LINE, "Drop to mention", shown, theme)
                .drag_over::<SidebarSessionDrag>(move |style, _, window, _| over(style, window)),
        )
    }
}

// slopty: the drop target's values are opencode's dropzone (`.research/opencode-study-2026-10-19.md`
// item 7): a 25 % veil under a wash of 4 % ink (6 % in the dark), arriving over 200 ms on an
// ease-out cubic; stripes 12 on 12 off at 135°, of 7 % ink (14 %), drifting one period in 3 s,
// held to the reading column (faded at its sides from 12 % to 88 %) inside an ellipse of
// 59 % × 40 %; a 40 px badge rising 4 px from .92 of its size. Neutral ink only.
/// The drop target's arrival.
const DROP_IN: crate::motion::MotionSpec =
    crate::motion::MotionSpec::new(200, crate::motion::CubicBezier::new(0.215, 0.61, 0.355, 1.0));
/// A stripe's width, and the gap after it.
const STRIPE: f32 = 12.0;
/// How long the field takes to drift one period.
const DRIFT: std::time::Duration = std::time::Duration::from_secs(3);
/// The badge's diameter, arrived.
const BADGE: f32 = 40.0;
/// How far the badge and its words rise into place.
const RISE: f32 = 4.0;

thread_local! {
    /// The drop targets a drag lay over in the frame drawn last, by key: a target arrives from
    /// the frame after a drag came over it, and leaves once it has gone.
    static OVER: std::cell::RefCell<std::collections::HashSet<&'static str>> =
        std::cell::RefCell::default();
}

/// The mention target's key beside the attachment target's of the same chat.
fn mention_key(key: &'static str) -> &'static str {
    match key {
        "chat" => "chat-mention",
        _ => "side-chat-mention",
    }
}

/// How far target `key` has arrived, 0 to 1: toward 1 while the frame drawn last had a drag
/// over it, back toward 0 once it left. Each frame asks afresh.
fn drop_target_shown(key: &'static str, cx: &App) -> f32 {
    let over = OVER.with(|over| over.borrow_mut().remove(key));
    crate::motion::state_t(
        &format!("drop-target-{key}"),
        over,
        DROP_IN,
        crate::motion::reduced_motion(cx),
        cx.frame_time(),
    )
}

/// The drag-over hook of target `key`: it notes that a drag lies over the target and asks for
/// the frame that shows it.
fn drop_target_over(
    key: &'static str,
) -> impl Fn(gpui::StyleRefinement, &mut Window) -> gpui::StyleRefinement + Copy {
    move |style, window| {
        if OVER.with(|over| over.borrow_mut().insert(key)) {
            window.request_animation_frame();
        }
        style
    }
}

/// The target itself, at `shown` of its arrival.
fn drop_target(
    key: &'static str,
    glyph: &'static str,
    words: &'static str,
    shown: f32,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let rise = RISE * (1.0 - shown);
    let badge = BADGE * (0.92 + 0.08 * shown);
    let dark = matches!(theme.appearance, crate::theme::Appearance::Dark);
    div()
        .id(if key.ends_with("mention") { "mention-drop-overlay" } else { "attachment-drop-overlay" })
        .absolute()
        .inset_0()
        .opacity(shown)
        .bg(theme.scrim().opacity(0.25 / 0.6))
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(crate::theme::ink(if dark { 0.06 } else { 0.04 })),
        )
        .when(shown > 0.0, |target| target.child(stripe_field(dark)))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(Theme::SPACE_SM))
                .child(
                    div()
                        .relative()
                        .top(px(rise))
                        .size(px(badge))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(crate::popover::surface_bg(theme))
                        .border_1()
                        .border_color(theme.border)
                        .shadow_md()
                        .child(
                            crate::icons::icon(glyph)
                                .size(px(18.0 * badge / BADGE))
                                .text_color(theme.text),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .top(px(rise))
                        .text_size(crate::typography::ui_rems(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(words),
                ),
        )
}

/// The field of faint 135° stripes, drifting on the shared clock, held to the reading column
/// and an ellipse at its middle; still under reduced motion.
fn stripe_field(dark: bool) -> gpui::Canvas<()> {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, (), window, cx| {
            let drift = if crate::motion::reduced_motion(cx) {
                0.0
            } else {
                window.request_animation_frame();
                crate::motion::locked_phase(DRIFT, window.frame_time()) * STRIPE * 2.0
            };
            let ink = if dark { 0.14 } else { 0.07 };
            for (quad, alpha) in stripe_quads(bounds, drift) {
                let mut path = gpui::PathBuilder::fill();
                path.move_to(quad[0]);
                for corner in &quad[1..] {
                    path.line_to(*corner);
                }
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, crate::theme::ink(alpha * ink));
                }
            }
        },
    )
    .absolute()
    .inset_0()
}

/// The stripes' pieces over `bounds`, `drift` on across them: each a quad and how much of the
/// field's ink it takes there, the column's side fades times the ellipse's.
fn stripe_quads(
    bounds: gpui::Bounds<Pixels>,
    drift: f32,
) -> Vec<([gpui::Point<Pixels>; 4], f32)> {
    /// How long a piece of stripe is: short enough that its ink follows the masks.
    const PIECE: f32 = 24.0;
    let column = f32::from(bounds.size.width).min(crate::composer::COMPOSER_MAX_WIDTH);
    let (mid_x, mid_y) = (f32::from(bounds.center().x), f32::from(bounds.center().y));
    let (rx, ry) = (column * 0.59, f32::from(bounds.size.height) * 0.40);
    let left = mid_x - column / 2.0;
    let alpha = |x: f32, y: f32| {
        let across = ((x - left) / column).clamp(0.0, 1.0);
        let sides = (across / 0.12).min((1.0 - across) / 0.12).clamp(0.0, 1.0);
        let inside = (1.0 - ((x - mid_x) / rx).hypot((y - mid_y) / ry)).clamp(0.0, 1.0);
        sides * inside * inside * (3.0 - 2.0 * inside)
    };
    // (u, v): u across the stripes, v along them, both from the field's middle.
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let at = |u: f32, v: f32| (mid_x + (u + v) * s, mid_y + (u - v) * s);
    let corner = |(x, y): (f32, f32)| gpui::point(px(x), px(y));
    let period = STRIPE * 2.0;
    let reach = (column / 2.0).hypot(ry);
    let mut quads = Vec::new();
    let mut u = -(reach / period).ceil() * period + drift.rem_euclid(period);
    while u < reach {
        let mut v = -reach;
        while v < reach {
            let (x, y) = at(u + STRIPE / 2.0, v + PIECE / 2.0);
            let a = alpha(x, y);
            if a > 0.005 {
                quads.push((
                    [
                        corner(at(u, v)),
                        corner(at(u + STRIPE, v)),
                        corner(at(u + STRIPE, v + PIECE)),
                        corner(at(u, v + PIECE)),
                    ],
                    a,
                ));
            }
            v += PIECE;
        }
        u += period;
    }
    quads
}

#[cfg(test)]
mod tests {
    use super::*;

    /// slopty: the field holds to the reading column, fading to nothing at its sides, and it
    /// drifts by its stripes' own period, so a whole period on it draws the field it started
    /// from; between, it has moved.
    #[test]
    fn the_stripe_field_holds_to_the_column_and_drifts_by_its_period() {
        let bounds =
            gpui::Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(px(1200.0), px(800.0)));
        let quads = stripe_quads(bounds, 0.0);
        assert!(quads.len() > 50, "a field, not a few marks: {}", quads.len());
        let half = crate::composer::COMPOSER_MAX_WIDTH / 2.0;
        for (quad, alpha) in &quads {
            assert!(*alpha > 0.0 && *alpha <= 1.0);
            let x = quad.iter().map(|corner| f32::from(corner.x)).sum::<f32>() / 4.0;
            assert!((x - 600.0).abs() < half, "inside the column: {x}");
        }
        let field = |drift: f32| {
            stripe_quads(bounds, drift)
                .iter()
                .map(|(quad, alpha)| (f32::from(quad[0].x).round() as i32, (alpha * 1000.0).round() as i32))
                .collect::<Vec<_>>()
        };
        assert_eq!(field(0.0), field(STRIPE * 2.0), "one period on, the same field");
        assert_ne!(field(0.0), field(STRIPE), "half a period on, the stripes lie between");
    }
}
