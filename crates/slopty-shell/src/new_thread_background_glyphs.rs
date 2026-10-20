//! slopty: the new-thread hero's glyph field, drawn.
//!
//! One view over the chat column, under everything on it, shown while the hero is up and no
//! wallpaper is set. It asks its timeline ([`Schedule`]) what to draw and when to draw again,
//! lights its cells ([`light`]) and paints each lit cell as one glyph of the theme's
//! monospace font through GPUI's glyph path, the one the terminal paints its screens with.
//! It hears the pointer over the column, warming the air along its path and rippling it on a
//! click outside the controls, and the keyboard, which keeps it moving.

use std::cell::Cell;
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, AppContext as _, Bounds, Context, DispatchPhase, Entity, FontId, GlyphId, Hsla, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Render, RenderImage, Rgba, SharedString, Styled,
    Subscription, Task, TextRun, Window, div, point, px,
};

use crate::new_thread_background_field::{
    BASE, Frame, GLYPH_SIZE, Grid, HEADROOM, Hand, Look, Moment, Pane, Visit, light,
};
use crate::new_thread_background_backdrop as backdrop;
use crate::new_thread_background_mask::SurfaceBounds;
use crate::new_thread_background_schedule::{Inputs, Schedule, Shown};

/// The seed of the one still frame drawn under reduced motion: a composed frame, the same each
/// time, with gusts on both sides of the composer and none over it.
const STILL_SEED: u32 = 0x5107_7e11;
/// How far the ink is cooled toward the accent.
const INK_COOL: f32 = 0.3;
/// How many steps a cell's colour takes from the ink to the glow.
const TINTS: usize = 16;
/// Below this a cell's ink cannot be seen on any surface, and it is not drawn.
const INVISIBLE: f32 = 1.0 / 255.0;
/// The band at the top of the column where the titlebar's controls sit: a click there is the
/// titlebar's.
const TITLEBAR_BAND: f32 = crate::theme::Theme::TITLEBAR_HEIGHT + crate::theme::Theme::TITLEBAR_TOP_PAD;
/// How far around the composer a click is the composer's, and the field is clear.
const COMPOSER_MARGIN: f32 = 8.0;
/// The characters' strength on a dark appearance, against a light one's.
const DARK_INK: f32 = 0.65;
/// The backdrop's alpha at a glow's heart: fainter in the dark, where a little light goes far,
/// as opencode keeps its dark hero.
const BACKDROP_DARK: f32 = 0.06;
const BACKDROP_LIGHT: f32 = 0.08;

/// The field's surroundings, as the shell renders them.
#[derive(Clone)]
pub(crate) struct Scene {
    /// The hero is up and nothing else is drawn on it: no chat selected, no wallpaper.
    pub hero: bool,
    /// The composer's surface, where the field is clear and the calm is centred.
    pub composer: SurfaceBounds,
    /// The characters' ink, and a ripple's glow, which is also the backdrop's cool light.
    pub ink: Hsla,
    pub glow: Hsla,
    /// The backdrop's ember, low on the left.
    pub warm: Hsla,
    /// A dark appearance, where the backdrop is fainter.
    pub dark: bool,
    /// The family the characters are drawn in.
    pub font: SharedString,
}

impl PartialEq for Scene {
    fn eq(&self, other: &Self) -> bool {
        self.hero == other.hero
            && Rc::ptr_eq(&self.composer, &other.composer)
            && self.ink == other.ink
            && self.glow == other.glow
            && self.warm == other.warm
            && self.dark == other.dark
            && self.font == other.font
    }
}

/// The field, kept in the shell's element state ([`gpui::Window::use_keyed_state`]) so the
/// shell needs no field of its own; the field itself is a view, so its frames draw it alone.
pub(crate) struct Slot(pub Entity<NewThreadField>);

/// The characters of a look in one font and size.
struct Glyphs {
    key: (SharedString, Look, u32),
    font: FontId,
    ids: Vec<GlyphId>,
    size: Pixels,
    advance: f32,
    ascent: f32,
    descent: f32,
}

/// What a frame was lit for, so a frame drawn again at the same moment is not lit again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LitFor {
    grid: Grid,
    pane: Pane,
    moment: Moment,
    seed: u32,
    marks: usize,
}

/// The backdrop made for a scene's colours, and the colours it was made for.
struct Backdrop {
    key: (Hsla, Hsla, bool),
    image: Arc<RenderImage>,
}

/// The living glyph field behind the new-thread hero.
pub(crate) struct NewThreadField {
    scene: Option<Scene>,
    schedule: Schedule,
    shown: Shown,
    visit: Option<Visit>,
    last_look: Option<Look>,
    /// The visit's clock starts here.
    origin: Instant,
    hand: Hand,
    /// How many marks the hand has made, so a new one lights the frame again.
    marks: usize,
    frame: Frame,
    lit_for: Option<LitFor>,
    glyphs: Option<Glyphs>,
    backdrop: Option<Backdrop>,
    /// Where the field was last painted, in window coordinates.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Increased contrast is on: the field is not drawn.
    contrast: bool,
    ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl NewThreadField {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe_window_activation(window, |this, window, cx| {
                // Coming back is when the contrast setting can have changed.
                if window.is_window_active() {
                    this.contrast = increased_contrast();
                }
                cx.notify();
            }),
            cx.observe_window_visibility(window, |_, _, _, cx| cx.notify()),
            // A key keeps the field moving, as in Claude desktop: no light of its own.
            cx.observe_keystrokes(|this, _, _, cx| this.stir(cx)),
        ];
        Self {
            scene: None,
            schedule: Schedule::default(),
            shown: Shown { opacity: 0.0, wind: 0.0, reveal: 0.0 },
            visit: None,
            last_look: None,
            origin: crate::clock::now(cx),
            hand: Hand::default(),
            marks: 0,
            frame: Frame::default(),
            lit_for: None,
            glyphs: None,
            backdrop: None,
            bounds: Rc::default(),
            contrast: increased_contrast(),
            ticker: None,
            _subscriptions: subscriptions,
        }
    }

    /// The shell's render hands the field its surroundings.
    pub(crate) fn set_scene(&mut self, scene: Scene, cx: &mut Context<Self>) {
        if self.scene.as_ref() != Some(&scene) {
            self.scene = Some(scene);
            cx.notify();
        }
    }

    /// Whether the field draws anything, or is fading out: the shell keeps it mounted until
    /// it has gone.
    pub(crate) fn showing(&self) -> bool {
        self.shown.opacity > 0.0
    }

    #[cfg(test)]
    pub(crate) fn set_contrast(&mut self, contrast: bool, cx: &mut Context<Self>) {
        self.contrast = contrast;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn shown(&self) -> Shown {
        self.shown
    }

    #[cfg(test)]
    pub(crate) fn ticking(&self) -> bool {
        self.ticker.is_some()
    }

    fn inputs(&self, window: &Window, cx: &App) -> Inputs {
        let now = self.visit_now(crate::clock::now(cx));
        Inputs {
            hero: self.scene.as_ref().is_some_and(|scene| scene.hero) && !self.contrast,
            visible: window.is_visible(),
            active: window.is_window_active(),
            reduced: cx.reduce_motion(),
            rippling: self.hand.rippling(now),
        }
    }

    fn visit_now(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.origin).as_secs_f32()
    }

    /// The pointer or a key stirred the field: it animates on, and wakes if it rested.
    fn stir(&mut self, cx: &mut Context<Self>) {
        if self.shown.opacity > 0.0 && self.ticker.is_none() {
            self.schedule.stir();
            cx.notify();
        } else {
            self.schedule.stir();
        }
    }

    fn moved(&mut self, at: (f32, f32), cx: &mut Context<Self>) {
        let now = self.visit_now(crate::clock::event_now());
        if self.hand.moved(at.0, at.1, now) {
            self.marks = self.marks.wrapping_add(1);
        }
        self.stir(cx);
    }

    fn tapped(&mut self, at: (f32, f32), cx: &mut Context<Self>) {
        let now = self.visit_now(crate::clock::event_now());
        self.hand.tapped(at.0, at.1, now);
        self.marks = self.marks.wrapping_add(1);
        self.schedule.stir();
        // A ripple draws at the display's rate from its first frame.
        cx.notify();
    }

    /// Start a visit when the field first shows: new weather, or under reduced motion the one
    /// composed still.
    fn begin_visit(&mut self, now: Instant, reduced: bool) {
        let seed = if reduced { STILL_SEED } else { random_seed() };
        let visit = Visit::new(seed, if reduced { None } else { self.last_look });
        self.last_look = Some(visit.look);
        self.visit = Some(visit);
        self.origin = now;
        self.hand = Hand::default();
        self.lit_for = None;
    }

    /// Ask for the next frame the timeline needs, if any.
    fn schedule_next(&mut self, now: Instant, inputs: Inputs, window: &mut Window, cx: &mut Context<Self>) {
        match self.schedule.next_frame(now, inputs) {
            Some(wait) if wait <= crate::new_thread_background_schedule::QUICK_FRAME => {
                self.ticker = None;
                window.request_animation_frame();
            }
            Some(wait) => {
                self.ticker = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    this.update(cx, |this, cx| {
                        this.ticker = None;
                        cx.notify();
                    })
                    .ok();
                }));
            }
            None => self.ticker = None,
        }
    }

    fn glyphs(&mut self, family: &SharedString, look: Look, window: &Window) -> Option<&Glyphs> {
        let size = px(GLYPH_SIZE);
        let key = (family.clone(), look, (GLYPH_SIZE * 64.0) as u32);
        if self.glyphs.as_ref().is_none_or(|glyphs| glyphs.key != key) {
            let text: SharedString = look.characters().into();
            let run = TextRun {
                len: text.len(),
                font: gpui::font(family.clone()),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = window.text_system().shape_line(text, size, &[run], None);
            let run = line.runs.first()?;
            let ids: Vec<GlyphId> = run.glyphs.iter().map(|glyph| glyph.id).collect();
            let count = ids.len().max(1) as f32;
            self.glyphs = Some(Glyphs {
                key,
                font: run.font_id,
                ids,
                size,
                advance: f32::from(line.width) / count,
                ascent: f32::from(line.ascent),
                descent: f32::from(line.descent),
            });
        }
        self.glyphs.as_ref()
    }

    /// Light the field for `bounds` and paint it.
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        self.bounds.set(Some(bounds));
        let (Some(scene), Some(visit)) = (self.scene.clone(), self.visit) else {
            return;
        };
        let shown = self.shown;
        if shown.opacity <= 0.0 {
            return;
        }
        let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let Some(grid) = Grid::new(width, height, window.scale_factor()) else {
            return;
        };
        let composer = scene.composer.get().map(|composer| {
            Bounds::new(
                point(composer.origin.x - bounds.origin.x, composer.origin.y - bounds.origin.y),
                composer.size,
            )
        });
        // The calm sits behind the composer; with none, where Claude puts its start card.
        let calm = composer.map_or((0.5 * width, 0.45 * height), |c| {
            (f32::from(c.center().x), f32::from(c.center().y))
        });
        let pane = Pane { width, height, calm };
        let moment = Moment {
            wind: shown.wind,
            now: self.visit_now(crate::clock::now(cx)),
            reveal: shown.reveal,
        };
        self.paint_backdrop(&scene, bounds, window);
        let lit_for = LitFor { grid, pane, moment, seed: visit.seed, marks: self.marks };
        if self.lit_for != Some(lit_for) {
            self.hand.prune(moment.now);
            light(&visit, &grid, &pane, &self.hand, moment, &mut self.frame);
            self.lit_for = Some(lit_for);
        }

        // The composer is opaque to the field: no cell under it is drawn.
        let clear = composer.map(|c| {
            let margin = px(2.0);
            let left = f32::from(c.origin.x - margin) - grid.origin.0;
            let right = f32::from(c.origin.x + c.size.width + margin) - grid.origin.0;
            let top = f32::from(c.origin.y - margin) - grid.origin.1;
            let bottom = f32::from(c.origin.y + c.size.height + margin) - grid.origin.1;
            (
                (left / grid.cell.0).floor() as isize,
                (right / grid.cell.0).ceil() as isize,
                (top / grid.cell.1).floor() as isize,
                (bottom / grid.cell.1).ceil() as isize,
            )
        });
        let Some(glyphs) = self.glyphs(&scene.font, visit.look, window) else {
            return;
        };
        let (font, size, advance) = (glyphs.font, glyphs.size, glyphs.advance);
        let baseline = (grid.cell.1 - (glyphs.ascent + glyphs.descent)) / 2.0 + glyphs.ascent;
        let ids = glyphs.ids.clone();
        let tints = tint_ramp(scene.ink, scene.glow);
        // The fade is the element's opacity (`render`), which the backdrop shares.
        // Claude draws both appearances alike; opencode keeps its dark hero far below its light
        // one, and on this near-black page the same ink reads brighter than on white.
        let strength = (BASE * HEADROOM).min(1.0) * if scene.dark { DARK_INK } else { 1.0 };
        let seen = shown.opacity * strength;
        for row in 0..grid.rows {
            let y = f32::from(bounds.origin.y) + grid.origin.1 + row as f32 * grid.cell.1 + baseline;
            for column in 0..grid.columns {
                let Some(lit) = self.frame.cells.get(row * grid.columns + column) else {
                    continue;
                };
                if lit.alpha * seen < INVISIBLE {
                    continue;
                }
                let alpha = lit.alpha * strength;
                if clear.is_some_and(|(l, r, t, b)| {
                    (l..r).contains(&(column as isize)) && (t..b).contains(&(row as isize))
                }) {
                    continue;
                }
                let Some(&id) = ids.get(usize::from(lit.glyph)) else {
                    continue;
                };
                let tint = ((lit.tint * (TINTS - 1) as f32).round() as usize).min(TINTS - 1);
                let color = tints.get(tint).copied().unwrap_or(scene.ink).opacity(alpha);
                let x = f32::from(bounds.origin.x)
                    + grid.origin.0
                    + column as f32 * grid.cell.0
                    + (grid.cell.0 - advance) / 2.0;
                let _ = window.paint_glyph(point(px(x), px(y)), font, id, size, color);
            }
        }
    }

    /// The warm and cool light beneath the characters, made once for the scene's colours.
    fn paint_backdrop(&mut self, scene: &Scene, bounds: Bounds<Pixels>, window: &mut Window) {
        let key = (scene.warm, scene.glow, scene.dark);
        if self.backdrop.as_ref().is_none_or(|backdrop| backdrop.key != key) {
            let rgb = |color: Hsla| {
                let color = Rgba::from(color);
                [color.r * 255.0, color.g * 255.0, color.b * 255.0]
            };
            let strength = if scene.dark { BACKDROP_DARK } else { BACKDROP_LIGHT };
            let pixels = backdrop::backdrop(rgb(scene.warm), rgb(scene.glow), strength);
            let image = Arc::new(RenderImage::new([image::Frame::new(pixels)]));
            if let Some(old) = self.backdrop.replace(Backdrop { key, image }) {
                let _ = window.drop_image(old.image);
            }
        }
        let Some(backdrop) = &self.backdrop else {
            return;
        };
        // The image's border lies just outside the pane, so the pane's edge samples only the
        // backdrop's own texels.
        let size = backdrop.image.size(0);
        let border = backdrop::BORDER as f32;
        let across = bounds.size.width / (size.width.0 as f32 - 2.0 * border) * border;
        let down = bounds.size.height / (size.height.0 as f32 - 2.0 * border) * border;
        let image_bounds =
            bounds.extend(gpui::Edges { top: down, right: across, bottom: down, left: across });
        let _ = window.paint_image(
            bounds,
            image_bounds,
            gpui::Corners::default(),
            backdrop.image.clone(),
            0,
            false,
        );
    }

    /// Hear the pointer over the field while it animates.
    fn listen(entity: &Entity<Self>, bounds: Bounds<Pixels>, composer: &SurfaceBounds, window: &mut Window) {
        let mover = entity.downgrade();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            mover
                .update(cx, |this, cx| {
                    if bounds.contains(&event.position) {
                        let at = event.position - bounds.origin;
                        this.moved((f32::from(at.x), f32::from(at.y)), cx);
                    } else {
                        this.hand.left();
                    }
                })
                .ok();
        });
        let clicker = entity.downgrade();
        let composer = composer.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            // A click outside the controls: anything that took it for itself (a field, a
            // focusable control) has prevented the default by now, on its way out.
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || window.default_prevented()
                || !bounds.contains(&event.position)
            {
                return;
            }
            let at = event.position - bounds.origin;
            if f32::from(at.y) < TITLEBAR_BAND {
                return;
            }
            if composer.get().is_some_and(|c| c.dilate(px(COMPOSER_MARGIN)).contains(&event.position)) {
                return;
            }
            clicker.update(cx, |this, cx| this.tapped((f32::from(at.x), f32::from(at.y)), cx)).ok();
        });
    }
}

impl Render for NewThreadField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::clock::now(cx);
        let inputs = self.inputs(window, cx);
        self.shown = self.schedule.advance(now, inputs);
        if self.schedule.take_ended() {
            self.visit = None;
        }
        if self.shown.opacity > 0.0 && self.visit.is_none() {
            self.begin_visit(now, inputs.reduced);
        }
        self.schedule_next(now, inputs, window, cx);
        let entity = cx.entity();
        let composer = self.scene.as_ref().map(|scene| scene.composer.clone());
        let touchable = self.shown.opacity > 0.0 && !inputs.reduced;
        div().absolute().inset_0().opacity(self.shown.opacity).child(
            gpui::canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    if touchable && let Some(composer) = &composer {
                        Self::listen(&entity, bounds, composer, window);
                    }
                    entity.update(cx, |this, cx| this.paint(bounds, window, cx));
                },
            )
            .absolute()
            .inset_0(),
        )
    }
}

/// The field of the window's new-thread hero, handed its surroundings: `hero` while the hero
/// is up with no wallpaper on it. It lives in the shell's element state for as long as the
/// shell renders.
pub(crate) fn field(
    hero: bool,
    composer: SurfaceBounds,
    theme: &crate::theme::Theme,
    window: &mut Window,
    cx: &mut App,
) -> Entity<NewThreadField> {
    let slot = window.use_keyed_state("new-thread-field", cx, |window, cx| {
        Slot(cx.new(|cx| NewThreadField::new(window, cx)))
    });
    let field = slot.read(cx).0.clone();
    let scene = Scene {
        hero,
        composer,
        ink: ink(theme),
        glow: theme.accent,
        warm: ember(theme),
        dark: theme.appearance.is_dark(),
        font: theme.font_mono.clone(),
    };
    field.update(cx, |field, cx| field.set_scene(scene, cx));
    field
}

/// The characters' ink: the theme's faint text, cooled toward its accent, so a gust reads as
/// light on the page rather than as grey text.
fn ink(theme: &crate::theme::Theme) -> Hsla {
    let (faint, accent) = (Rgba::from(theme.text_faint), Rgba::from(theme.accent));
    let cool = INK_COOL;
    Hsla::from(Rgba {
        r: faint.r + (accent.r - faint.r) * cool,
        g: faint.g + (accent.g - faint.g) * cool,
        b: faint.b + (accent.b - faint.b) * cool,
        a: 1.0,
    })
}

/// The backdrop's ember: the hue between the theme's danger and warning, so it follows the
/// theme and reads as warm light, never as a status.
fn ember(theme: &crate::theme::Theme) -> Hsla {
    let (danger, warning) = (theme.danger.h, theme.warning.h);
    // The shorter way round the hue circle.
    let mut turn = warning - danger;
    if turn > 0.5 {
        turn -= 1.0;
    } else if turn < -0.5 {
        turn += 1.0;
    }
    gpui::hsla((danger + turn / 2.0).rem_euclid(1.0), 0.9, 0.62, 1.0)
}

/// The colours from the characters' ink to a ripple's glow, in even steps.
fn tint_ramp(ink: Hsla, glow: Hsla) -> [Hsla; TINTS] {
    let (a, b) = (Rgba::from(ink), Rgba::from(glow));
    std::array::from_fn(|i| {
        let t = i as f32 / (TINTS - 1) as f32;
        Hsla::from(Rgba {
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: a.a + (b.a - a.a) * t,
        })
    })
}

/// A seed for a new visit's weather.
fn random_seed() -> u32 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u8(0);
    hasher.finish() as u32
}

/// Whether the person asked the system for more contrast: the field is decoration, and goes.
#[cfg(all(target_os = "macos", not(test)))]
fn increased_contrast() -> bool {
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    // SAFETY: `NSWorkspace.sharedWorkspace` and its accessibility display flags are plain
    // getters, called on the main thread like every view's render.
    let increase: Bool = unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        msg_send![workspace, accessibilityDisplayShouldIncreaseContrast]
    };
    increase.as_bool()
}

/// Whether the person asked the system for more contrast ("Increase Contrast" on iPhone and
/// iPad): the field is decoration, and goes.
#[cfg(all(target_os = "ios", not(test)))]
fn increased_contrast() -> bool {
    #[link(name = "UIKit", kind = "framework")]
    unsafe extern "C" {
        fn UIAccessibilityDarkerSystemColorsEnabled() -> bool;
    }
    // SAFETY: a UIKit accessibility getter with no arguments, called on the main thread.
    unsafe { UIAccessibilityDarkerSystemColorsEnabled() }
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
const fn increased_contrast() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use gpui::{TestAppContext, VisualTestContext};
    use std::time::Duration;

    /// A window holding nothing but the field, the hero up.
    struct Host {
        hero: bool,
        composer: SurfaceBounds,
        field: Option<Entity<NewThreadField>>,
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::dark();
            let field = field(self.hero, self.composer.clone(), &theme, window, cx);
            self.field = Some(field.clone());
            div().size_full().child(field)
        }
    }

    fn host(cx: &mut TestAppContext, reduced: bool) -> (Entity<Host>, &mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            cx.set_reduce_motion(reduced);
        });
        let (host, cx) = cx.add_window_view(|_, _| Host { hero: true, composer: Rc::default(), field: None });
        cx.simulate_resize(gpui::size(px(1000.0), px(700.0)));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (host, cx)
    }

    fn field_of(host: &Entity<Host>, cx: &mut VisualTestContext) -> Entity<NewThreadField> {
        cx.update(|_, cx| host.read(cx).field.clone().expect("the host drew its field"))
    }

    fn wait(cx: &mut VisualTestContext, seconds: f32) {
        cx.executor().advance_clock(Duration::from_secs_f32(seconds));
        cx.run_until_parked();
    }

    /// The field waits out its delay, fades in on its own frames, and in the background holds
    /// its frame and asks for none until the window is in front again.
    #[gpui::test]
    fn a_window_in_the_background_asks_the_field_for_no_frames(cx: &mut TestAppContext) {
        let (host, cx) = host(cx, false);
        let field = field_of(&host, cx);
        cx.update(|_, cx| assert_eq!(field.read(cx).shown().opacity, 0.0, "nothing before 3 s"));
        wait(cx, 3.5);
        cx.update(|_, cx| {
            let field = field.read(cx);
            assert!(field.shown().opacity > 0.0, "fading in after 3 s");
            assert!(field.ticking(), "drawing its own frames");
        });
        cx.deactivate_window();
        let paused = cx.update(|_, cx| field.read(cx).shown());
        cx.update(|_, cx| assert!(!field.read(cx).ticking(), "no timer in the background"));
        wait(cx, 10.0);
        cx.update(|_, cx| {
            assert!(!field.read(cx).ticking());
            assert_eq!(field.read(cx).shown().wind, paused.wind, "the wind stands still");
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.update(|_, cx| assert!(field.read(cx).ticking(), "in front again, it moves on"));
    }

    /// Under reduced motion the field is one still frame from the first, asking for nothing.
    #[gpui::test]
    fn reduced_motion_holds_the_field_still(cx: &mut TestAppContext) {
        let (host, cx) = host(cx, true);
        let field = field_of(&host, cx);
        let first = cx.update(|_, cx| field.read(cx).shown());
        assert_eq!(first.opacity, 1.0);
        cx.update(|_, cx| assert!(!field.read(cx).ticking()));
        wait(cx, 30.0);
        cx.update(|_, cx| assert_eq!(field.read(cx).shown(), first));
    }

    /// Under increased contrast the field is not drawn at all.
    #[gpui::test]
    fn increased_contrast_takes_the_field_away(cx: &mut TestAppContext) {
        let (host, cx) = host(cx, true);
        let field = field_of(&host, cx);
        cx.update(|_, cx| field.update(cx, |field, cx| field.set_contrast(true, cx)));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(field.read(cx).shown().opacity, 0.0);
            assert!(!field.read(cx).showing());
        });
    }

    /// The first send takes the hero away: the field fades out and then asks for nothing.
    #[gpui::test]
    fn leaving_the_hero_fades_the_field_out(cx: &mut TestAppContext) {
        let (host, cx) = host(cx, false);
        let field = field_of(&host, cx);
        wait(cx, 7.0);
        cx.update(|_, cx| host.update(cx, |host, cx| {
            host.hero = false;
            cx.notify();
        }));
        cx.run_until_parked();
        cx.update(|_, cx| assert!(field.read(cx).showing(), "still fading"));
        wait(cx, 0.6);
        // The display's next frame, which a headless window has none of.
        cx.update(|_, cx| field.update(cx, |_, cx| cx.notify()));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert!(!field.read(cx).showing(), "gone after 500 ms");
            assert!(!field.read(cx).ticking());
        });
    }
}
