//! The touch key row: the keys a soft keyboard lacks, on a strip just above it, at Blink Shell's
//! measure (`docs/decisions/layout.md`, "iPhone is a list, then the terminal"; Blink's
//! `SmarterKeys`).
//!
//! Esc, ⌃ and ⌥ stand fixed at the left; the arrows as one joystick, hide keyboard and ⌘ at the
//! right; between them a strip that scrolls. The strip leads with the keys of what Slopty's
//! records say runs in the session, an agent by its harness or a shell, then the symbols a phone
//! keyboard hides, each a two-symbol key whose flick down gives the second. ⌘ is a layer: while it
//! is on, the strip holds F1 to F12 and the joystick Home, End, Page Up and Page Down.
//!
//! A modifier taps for one key and holds for a chord (`live::key_row`, which the terminal spends
//! as keys go). Every other key goes into the window as the key itself, a keyboard's key down and
//! up: the terminal holding the keyboard takes it as it takes a hardware key, and only what the
//! person pressed is sent. The row stands only while the soft keyboard is up over the session's
//! terminal and no hardware keyboard is attached.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::clone_on_ref_ptr,
    clippy::field_scoped_visibility_modifiers,
    clippy::needless_pass_by_ref_mut,
    clippy::or_fun_call,
    clippy::suboptimal_flops,
    reason = "shell lint migration: slice A"
)]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, FocusHandle, Hsla, KeyUpEvent, Keystroke,
    Modifiers, Pixels, PlatformInput, Point, Render, SharedString, Subscription, Task,
    TouchDragEvent, TouchPhase, Window, canvas, div, px,
};
use slopty_view_proto::HarnessId;

use crate::icons::{self, icon};
use crate::live::key_row::{self as armed, Modifier};
use crate::theme::Theme;

/// A key's plate inside its column (Blink's key insets: 3 above, 1 on each side and below), its
/// corner, and its faces' sizes.
const PLATE_TOP: f32 = 3.0;
const PLATE_SIDE: f32 = 1.0;
const PLATE_BOTTOM: f32 = 1.0;
const PLATE_CORNER: f32 = 5.0;
const TEXT: f32 = 14.0;
const SYMBOL: f32 = 16.0;

/// The travel before a finger on the strip is read as a scroll (gpui's touch slop).
const SLOP: f32 = 8.0;
/// A two-symbol key's flick: the share of its travel (its height less 10 pt) past which the
/// second symbol goes, and past which the strip no longer scrolls under it (Blink's 0.75, 0.15).
const FLICK_AT: f32 = 0.75;
const FLICK_HOLDS: f32 = 0.15;
/// A held key's first repeat, and its pace after (Blink's 0.5 s, then 0.1 s).
const REPEAT_AFTER: Duration = Duration::from_millis(500);
const REPEAT_EVERY: Duration = Duration::from_millis(100);
/// The joystick's dead zone as a share of its plate (Blink's 20 % across, 15 % down), and the
/// faster paces a finger pushed further asks for: past 0.9 plates across or 3 down, 80 ms; past 2
/// across or 4 down, 50 ms.
const DEAD_ACROSS: f32 = 0.2;
const DEAD_DOWN: f32 = 0.15;
const FASTER: Duration = Duration::from_millis(80);
const FASTEST: Duration = Duration::from_millis(50);

/// The row's measure (Blink's `KBSizes` by device): its height, a key's column and a wide key's,
/// chosen so the keys stand on the system keyboard's own columns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measure {
    /// 44 pt in portrait, 38 in landscape.
    pub(crate) height: f32,
    /// A key's column.
    pub(crate) key: f32,
    /// A wide key's column: the joystick, ⌘, and Esc in landscape.
    pub(crate) wide: f32,
}

impl Measure {
    /// Whether it is landscape's, the shorter row.
    fn landscape(self) -> bool {
        self.height < 44.0
    }
}

/// The row's measure in a window `width` × `height` points: by orientation, then by the screen's
/// width, as Blink sizes it per phone.
pub fn measure(width: f32, height: f32) -> Measure {
    if height >= width {
        // The 6.7-inch phones (428 pt wide and more) take a point more a key.
        let big = width >= 428.0;
        return Measure {
            height: 44.0,
            key: if big { 34.0 } else { 33.0 },
            wide: if big { 40.0 } else { 39.0 },
        };
    }
    let (key, wide) = match width {
        w if w < 600.0 => (36.0, 46.0),
        w if w < 690.0 => (36.0, 51.5),
        w if w < 840.0 => (40.0, 62.5),
        w if w < 890.0 => (42.0, 64.0),
        w if w < 910.0 => (45.0, 66.5),
        _ => (47.0, 68.0),
    };
    Measure { height: 38.0, key, wide }
}

/// A keystroke a key sends: the key as gpui names it, the modifiers it carries, and whether it is
/// a character, typed as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    key: &'static str,
    control: bool,
    shift: bool,
    text: bool,
}

impl Press {
    const fn key(key: &'static str) -> Self {
        Self { key, control: false, shift: false, text: false }
    }

    const fn char(key: &'static str) -> Self {
        Self { key, control: false, shift: false, text: true }
    }

    const fn control(key: &'static str) -> Self {
        Self { key, control: true, shift: false, text: false }
    }

    const fn shift(key: &'static str) -> Self {
        Self { key, control: false, shift: true, text: false }
    }

    /// The keystroke as a keyboard would send it.
    pub(crate) fn keystroke(self) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                control: self.control,
                shift: self.shift,
                ..Modifiers::default()
            },
            key: self.key.to_owned(),
            key_char: self.text.then(|| self.key.to_owned()),
        }
    }

    /// Whether a held key sends it again (Blink's `shouldAutoRepeat`: Esc, Tab, the arrows).
    fn repeats(self) -> bool {
        !self.control && matches!(self.key, "escape" | "tab" | "up" | "down" | "left" | "right")
    }
}

/// What a single key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Sends a keystroke.
    Press(Press),
    /// Arms a modifier.
    Modifier(Modifier),
    /// Puts the soft keyboard away; a tap on the terminal brings it back.
    HideKeyboard,
}

/// What a key shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// Words or a glyph in the row's face.
    Text(&'static str),
    /// One of the shell's icons.
    Icon(&'static str),
}

/// A key on the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// One act: its face, its spoken name, and whether it takes a wide column.
    One { face: Face, name: &'static str, act: Act, wide: bool },
    /// Two symbols: a tap gives the first, a flick down the second (Blink's `vertical2`).
    Two { top: (&'static str, Press), below: (&'static str, Press) },
    /// The four arrows from one key, steered by where a finger presses and drags it.
    Arrows,
}

impl Key {
    const fn one(face: Face, name: &'static str, act: Act) -> Self {
        Self::One { face, name, act, wide: false }
    }

    const fn two(top: &'static str, below: &'static str) -> Self {
        Self::Two { top: (top, Press::char(top)), below: (below, Press::char(below)) }
    }

    /// Its column: a wide key's, or a key's.
    const fn width(self, measure: Measure) -> f32 {
        match self {
            Self::Arrows | Self::One { wide: true, .. } => measure.wide,
            Self::One { .. } | Self::Two { .. } => measure.key,
        }
    }

    /// What a screen reader calls it, and the tests find it by.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::One { name, .. } => name,
            Self::Two { top, .. } => top.0,
            Self::Arrows => "Arrows",
        }
    }
}

const ESC: Key = Key::one(Face::Text("esc"), "Escape", Act::Press(Press::key("escape")));
const ESC_WIDE: Key = Key::One {
    face: Face::Text("esc"),
    name: "Escape",
    act: Act::Press(Press::key("escape")),
    wide: true,
};
const CONTROL: Key = Key::one(Face::Text("\u{2303}"), "Control", Act::Modifier(Modifier::Control));
const OPTION: Key = Key::one(Face::Text("\u{2325}"), "Option", Act::Modifier(Modifier::Alt));
const COMMAND: Key = Key::One {
    face: Face::Text("\u{2318}"),
    name: "Command",
    act: Act::Modifier(Modifier::Command),
    wide: true,
};
const HIDE: Key = Key::one(Face::Icon(icons::KEYBOARD), "Hide keyboard", Act::HideKeyboard);
const TAB: Key = Key::one(Face::Text("\u{21e5}"), "Tab", Act::Press(Press::key("tab")));
const SHIFT_TAB: Key =
    Key::one(Face::Text("\u{21e7}\u{21e5}"), "Shift Tab", Act::Press(Press::shift("tab")));
const CONTROL_C: Key =
    Key::one(Face::Text("\u{2303}C"), "Control C", Act::Press(Press::control("c")));
const CONTROL_R: Key =
    Key::one(Face::Text("\u{2303}R"), "Control R", Act::Press(Press::control("r")));
const UP: Key = Key::one(Face::Icon(icons::ARROW_UP), "Up", Act::Press(Press::key("up")));
const SLASH: Key = Key::two("/", "?");
const AT: Key = Key::two("@", "#");

/// The symbols a phone keyboard hides, after the keys of what runs (Blink's iPhone middle).
const SYMBOLS: [Key; 13] = [
    TAB,
    Key::two("`", "~"),
    AT,
    Key::two("$", "^"),
    Key::two(";", ":"),
    Key::two("-", "_"),
    Key::two("=", "+"),
    Key::two("[", "{"),
    Key::two("]", "}"),
    Key::two("\\", "|"),
    Key::two("<", "*"),
    Key::two(">", "\""),
    SLASH,
];

/// The ⌘ layer's strip: F1 to F12, two to a key. The terminal sends them without the ⌘, as
/// Blink does.
const FUNCTIONS: [Key; 6] = [
    Key::Two { top: ("F1", Press::key("f1")), below: ("F7", Press::key("f7")) },
    Key::Two { top: ("F2", Press::key("f2")), below: ("F8", Press::key("f8")) },
    Key::Two { top: ("F3", Press::key("f3")), below: ("F9", Press::key("f9")) },
    Key::Two { top: ("F4", Press::key("f4")), below: ("F10", Press::key("f10")) },
    Key::Two { top: ("F5", Press::key("f5")), below: ("F11", Press::key("f11")) },
    Key::Two { top: ("F6", Press::key("f6")), below: ("F12", Press::key("f12")) },
];

/// Claude Code's keys: ⇧⇥ steps its mode, ⌃C clears or quits, `/` its commands, `@` a file, ↑
/// the last prompt. Esc, which stops it, stands fixed at the left, so the strip never repeats it.
const CLAUDE_CODE: [Key; 5] = [SHIFT_TAB, CONTROL_C, SLASH, AT, UP];
/// Any other agent's: the same, less Claude Code's mode key.
const AGENT: [Key; 4] = [CONTROL_C, SLASH, AT, UP];
/// A shell's: completion, history search, interrupt, the last command.
const SHELL: [Key; 4] = [TAB, CONTROL_R, CONTROL_C, UP];

/// The keys of what Slopty's records say runs in the session: the agent its chat was started
/// with, by harness, or with none a shell. Never read from the screen.
pub fn running(harness: Option<HarnessId>) -> &'static [Key] {
    match harness {
        Some(HarnessId::CLAUDE_CODE) => &CLAUDE_CODE,
        Some(_) => &AGENT,
        None => &SHELL,
    }
}

/// The strip: the ⌘ layer's function keys, or the keys of what runs then the symbols it lacks.
pub fn strip(running: &[Key], layer: bool) -> Vec<Key> {
    if layer {
        return FUNCTIONS.to_vec();
    }
    let mut keys = running.to_vec();
    keys.extend(SYMBOLS.iter().filter(|key| !running.contains(key)));
    keys
}

/// What an iPad keyboard's shortcut bar holds for a session that runs `harness` (an iPad laid out
/// as the Mac has the bar in place of the row): Esc and the three modifiers lead; the keys of
/// what runs, then the four arrows, trail. The bar takes taps only, so it keeps the keys that are
/// one act: the symbols stay on the iPad's keyboard, which shows them, and its own key puts the
/// keyboard away.
pub fn bar(harness: Option<HarnessId>) -> armed::Bar {
    let entry = |key: Key| {
        let Key::One { face: Face::Text(face), name, act, .. } = key else {
            return None;
        };
        let press = match act {
            Act::Press(press) => armed::BarPress::Key(press.keystroke()),
            Act::Modifier(modifier) => armed::BarPress::Modifier(modifier),
            Act::HideKeyboard => return None,
        };
        Some(armed::BarKey { face, name, press })
    };
    let arrows = Arrow::ALL.map(|arrow| armed::BarKey {
        face: arrow.glyph(),
        name: arrow.name(false),
        press: armed::BarPress::Key(arrow.press(false).keystroke()),
    });
    armed::Bar {
        leading: [ESC, CONTROL, OPTION, COMMAND].into_iter().filter_map(entry).collect(),
        trailing: running(harness).iter().copied().filter_map(entry).chain(arrows).collect(),
    }
}

/// The keys fixed at the left, then those fixed at the right; Esc is wide in landscape.
fn sides(measure: Measure) -> ([Key; 3], [Key; 3]) {
    let esc = if measure.landscape() { ESC_WIDE } else { ESC };
    ([esc, CONTROL, OPTION], [Key::Arrows, HIDE, COMMAND])
}

/// A key laid out: which, and its column's left edge and width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub(crate) key: Key,
    pub(crate) x: f32,
    pub(crate) width: f32,
}

/// The row laid out in its width: the keys at the sides from the row's left edge, the strip
/// between them, and the strip's keys from its own left edge.
#[derive(Clone, Debug, PartialEq)]
pub struct Laid {
    pub(crate) left: Vec<Placed>,
    pub(crate) right: Vec<Placed>,
    /// The strip's left edge and width.
    pub(crate) strip: (f32, f32),
    pub(crate) keys: Vec<Placed>,
    /// How far the strip scrolls: its keys' width past its own.
    pub(crate) overflow: f32,
}

/// Lay the row out across `width` (Blink's `KBView.layoutSubviews`): the left keys from the left
/// edge, the right ones from the right, the strip between, its keys abutting.
pub fn lay_out(width: f32, measure: Measure, strip: &[Key]) -> Laid {
    let (left_keys, right_keys) = sides(measure);
    let abutting = |keys: &[Key]| {
        let mut x = 0.0;
        keys.iter()
            .map(|&key| {
                let placed = Placed { key, x, width: key.width(measure) };
                x += placed.width;
                placed
            })
            .collect::<Vec<_>>()
    };
    let left = abutting(&left_keys);
    let strip_left: f32 = left.iter().map(|placed| placed.width).sum();
    let right_width: f32 = right_keys.iter().map(|key| key.width(measure)).sum();
    let right_edge = width - right_width;
    let right = abutting(&right_keys)
        .into_iter()
        .map(|placed| Placed { x: placed.x + right_edge, ..placed })
        .collect();
    let strip_width = (right_edge - strip_left).max(0.0);
    let keys = abutting(strip);
    let keys_width: f32 = keys.iter().map(|placed| placed.width).sum();
    Laid {
        left,
        right,
        strip: (strip_left, strip_width),
        keys,
        overflow: (keys_width - strip_width).max(0.0),
    }
}

/// Which key of the row stands at `x` from its left edge, with the strip scrolled by `scroll`:
/// the key placed at its column's left edge from the row's, and whether it is on the strip.
fn key_at(laid: &Laid, x: f32, scroll: f32) -> Option<(Placed, bool)> {
    let within = |placed: &&Placed, x: f32| x >= placed.x && x < placed.x + placed.width;
    if let Some(placed) = laid.left.iter().chain(&laid.right).find(|placed| within(placed, x)) {
        return Some((*placed, false));
    }
    let (left, width) = laid.strip;
    if x < left || x >= left + width {
        return None;
    }
    let inside = x - left + scroll;
    laid.keys
        .iter()
        .find(|placed| within(placed, inside))
        .map(|placed| (Placed { x: placed.x + left - scroll, ..*placed }, true))
}

/// One of the joystick's four ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Down,
    Up,
    Right,
}

impl Arrow {
    const ALL: [Self; 4] = [Self::Left, Self::Down, Self::Up, Self::Right];

    /// The key it sends: an arrow, or under ⌘ Home, Page Down, Page Up and End.
    const fn press(self, layer: bool) -> Press {
        match (self, layer) {
            (Self::Left, false) => Press::key("left"),
            (Self::Down, false) => Press::key("down"),
            (Self::Up, false) => Press::key("up"),
            (Self::Right, false) => Press::key("right"),
            (Self::Left, true) => Press::key("home"),
            (Self::Down, true) => Press::key("pagedown"),
            (Self::Up, true) => Press::key("pageup"),
            (Self::Right, true) => Press::key("end"),
        }
    }

    /// Its glyph, where it stands alone (an iPad's shortcut bar).
    const fn glyph(self) -> &'static str {
        match self {
            Self::Left => "\u{2190}",
            Self::Down => "\u{2193}",
            Self::Up => "\u{2191}",
            Self::Right => "\u{2192}",
        }
    }

    /// What a screen reader calls it.
    const fn name(self, layer: bool) -> &'static str {
        match (self, layer) {
            (Self::Left, false) => "Left",
            (Self::Down, false) => "Down",
            (Self::Up, false) => "Up",
            (Self::Right, false) => "Right",
            (Self::Left, true) => "Home",
            (Self::Down, true) => "Page Down",
            (Self::Up, true) => "Page Up",
            (Self::Right, true) => "End",
        }
    }
}

/// Where a finger at `offset` from the joystick's plate centre steers, the plate `size`, and how
/// fast it repeats there (Blink's `KBKeyViewArrows`): nothing inside the dead zone, else the
/// axis it leans along most.
pub fn steer(offset: Point<f32>, size: (f32, f32)) -> Option<(Arrow, Duration)> {
    let (width, height) = size;
    let (dx, dy) = (offset.x.abs(), offset.y.abs());
    if dx < width * DEAD_ACROSS && dy < height * DEAD_DOWN {
        return None;
    }
    let arrow = if dx > dy {
        if offset.x < 0.0 { Arrow::Left } else { Arrow::Right }
    } else if offset.y < 0.0 {
        Arrow::Up
    } else {
        Arrow::Down
    };
    let pace = if dx > width * 2.0 || dy > height * 4.0 {
        FASTEST
    } else if dx > width * 0.9 || dy > height * 3.0 {
        FASTER
    } else {
        REPEAT_EVERY
    };
    Some((arrow, pace))
}

/// What a finger on the row is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Doing {
    /// Pressing a key.
    Pressing,
    /// Flicking a two-symbol key down, this far along its travel (0 to 1).
    Flicking(f32),
    /// Scrolling the strip: the scroll at which the finger would stand back where it came down.
    Scrolling(f32),
    /// Steering the joystick: the way it points, if out of the dead zone, and its pace.
    Steering(Option<(Arrow, Duration)>),
}

/// The finger on the row.
#[derive(Clone, Copy, Debug)]
struct Touch {
    key: Key,
    /// Its column's left edge from the row's, when the finger came down.
    column: f32,
    on_strip: bool,
    start: Point<Pixels>,
    doing: Doing,
    /// The key went already, as the finger came down or as it repeated.
    sent: bool,
}

#[cfg(test)]
thread_local! {
    /// Tests lay the row out with the soft keyboard up.
    static FORCE_KEYBOARD: Cell<bool> = const { Cell::new(false) };
}

/// Whether the soft keyboard stands over the window: iOS's keyboard inset, or a test that says.
fn soft_keyboard_up(window: &Window) -> bool {
    #[cfg(test)]
    if FORCE_KEYBOARD.with(Cell::get) {
        return true;
    }
    cfg!(target_os = "ios") && window.insets().ime.bottom > Pixels::ZERO
}

/// The key row, as the phone's session page draws it above the soft keyboard.
pub struct KeyRow {
    /// What runs in the session, the ground it stands on, and the terminal whose keyboard it
    /// stands over: the shell's to say as it draws ([`Self::set`]).
    running: &'static [Key],
    ground: Hsla,
    terminal: Option<FocusHandle>,
    /// Its measure while it stands, as last drawn.
    shown: Option<Measure>,
    /// The strip's scroll.
    scroll: f32,
    touch: Option<Touch>,
    /// A held key sending itself again, or the joystick's repeat.
    repeat: Option<Task<()>>,
    /// Its bounds as last painted, which a finger is placed against.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _armed: Subscription,
    _hardware: Subscription,
}

impl KeyRow {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let row = cx.weak_entity();
        let armed = armed::observe(cx, move |cx| {
            let _gone = row.update(cx, |_, cx| cx.notify());
        });
        let row = cx.weak_entity();
        let hardware = armed::observe_hardware_keyboard(cx, move |cx| {
            let _gone = row.update(cx, |_, cx| cx.notify());
        });
        Self {
            running: &SHELL,
            ground: Theme::of(cx).session_ground(),
            terminal: None,
            shown: None,
            scroll: 0.0,
            touch: None,
            repeat: None,
            bounds: Rc::default(),
            _armed: armed,
            _hardware: hardware,
        }
    }

    /// What the shell draws the row for: what runs (its keys, [`running`]), the ground it stands
    /// on, and the terminal it types into, which holds the keyboard while the row stands.
    pub(crate) fn set(
        &mut self,
        running: &'static [Key],
        ground: Hsla,
        terminal: FocusHandle,
        cx: &mut Context<Self>,
    ) {
        let new_keys = !std::ptr::eq(self.running, running);
        if new_keys || self.ground != ground || self.terminal.as_ref() != Some(&terminal) {
            if new_keys {
                self.scroll = 0.0;
            }
            self.running = running;
            self.ground = ground;
            self.terminal = Some(terminal);
            cx.notify();
        }
    }

    /// Its height while it stands, as last drawn; 0 while it is away.
    pub(crate) fn height(&self) -> f32 {
        self.shown.map_or(0.0, |measure| measure.height)
    }

    /// Whether it stands: the soft keyboard is up over the session's terminal, which holds the
    /// keyboard, and no hardware keyboard stands in for it.
    fn stands(&self, window: &Window, cx: &App) -> bool {
        soft_keyboard_up(window)
            && !armed::hardware_keyboard(cx)
            && self.terminal.as_ref().is_some_and(|terminal| terminal.contains_focused(window, cx))
    }

    /// The ⌘ layer is on.
    fn layer(cx: &App) -> bool {
        armed::armed(cx).on(Modifier::Command)
    }

    fn laid(&self, width: f32, measure: Measure, cx: &App) -> Laid {
        lay_out(width, measure, &strip(self.running, Self::layer(cx)))
    }

    /// Send `press` into the window, as a keyboard's key down and up.
    fn send(press: Press, window: &Window, cx: &mut App) {
        let keystroke = press.keystroke();
        window.defer(cx, move |window, cx| {
            window.dispatch_keystroke(keystroke.clone(), cx);
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        });
    }

    /// A key's act, once: a press sent, a modifier tapped, the keyboard put away.
    fn act(act: Act, window: &Window, cx: &mut App) {
        match act {
            Act::Press(press) => Self::send(press, window, cx),
            Act::Modifier(modifier) => {
                armed::down(modifier, cx);
                armed::up(modifier, cx);
            }
            Act::HideKeyboard => window.dismiss_virtual_keyboard(),
        }
    }

    /// A press by a pointer, or a screen reader's activation: the key's tap. The joystick's
    /// ways take theirs one at a time ([`arrow_presses`]).
    fn clicked(key: Key, window: &Window, cx: &mut App) {
        match key {
            Key::One { act, .. } => Self::act(act, window, cx),
            Key::Two { top, .. } => Self::send(top.1, window, cx),
            Key::Arrows => {}
        }
    }

    /// One step of a finger on the row (gpui's touch drag, claimed as it comes down): whether it
    /// was the row's.
    fn touched(
        &mut self,
        event: &TouchDragEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let ours = match event.phase {
            TouchPhase::Started => self.finger_down(event, window, cx),
            TouchPhase::Moved => {
                self.finger_moved(event, window, cx);
                self.touch.is_some()
            }
            TouchPhase::Ended => self.finger_up(window, cx),
            TouchPhase::Cancelled => self.finger_cancelled(cx),
        };
        if ours {
            cx.notify();
        }
        ours
    }

    fn finger_down(
        &mut self,
        event: &TouchDragEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(bounds), Some(measure)) = (self.bounds.get(), self.shown) else { return false };
        if !bounds.contains(&event.start_position) {
            return false;
        }
        let laid = self.laid(f32::from(bounds.size.width), measure, cx);
        let x = f32::from(event.start_position.x - bounds.origin.x);
        let Some((placed, on_strip)) = key_at(&laid, x, self.scroll) else { return false };
        self.repeat = None;
        let mut touch = Touch {
            key: placed.key,
            column: placed.x,
            on_strip,
            start: event.start_position,
            doing: Doing::Pressing,
            sent: false,
        };
        match placed.key {
            Key::Arrows => {
                touch.doing = Doing::Steering(None);
                self.touch = Some(touch);
                self.steer_at(event.start_position, window, cx);
                return true;
            }
            Key::One { act: Act::Modifier(modifier), .. } => armed::down(modifier, cx),
            Key::One { act: Act::Press(press), .. } if press.repeats() => {
                // A fixed key goes as the finger comes down, as a keyboard's does; one on the
                // strip waits to see the finger is not scrolling it (UIKit's delayed touches),
                // and goes when it lifts or once it repeats.
                if !on_strip {
                    Self::send(press, window, cx);
                    touch.sent = true;
                }
                self.repeat_after(REPEAT_AFTER, window, cx);
            }
            Key::One { .. } | Key::Two { .. } => {}
        }
        self.touch = Some(touch);
        true
    }

    fn finger_moved(
        &mut self,
        event: &TouchDragEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(mut touch), Some(bounds), Some(measure)) =
            (self.touch, self.bounds.get(), self.shown)
        else {
            return;
        };
        let moved = event.position - touch.start;
        let (dx, dy) = (f32::from(moved.x), f32::from(moved.y));
        match touch.doing {
            Doing::Steering(_) => {
                self.steer_at(event.position, window, cx);
                return;
            }
            Doing::Scrolling(origin) => {
                let laid = self.laid(f32::from(bounds.size.width), measure, cx);
                self.scroll = (origin - dx).clamp(0.0, laid.overflow);
            }
            Doing::Pressing | Doing::Flicking(_) => {
                let holds =
                    matches!(touch.doing, Doing::Flicking(progress) if progress > FLICK_HOLDS);
                if touch.on_strip && !holds && dx.abs() >= SLOP && dx.abs() > dy.abs() {
                    // Sideways on the strip it scrolls, and the key under the finger goes unsent.
                    touch.doing = Doing::Scrolling(self.scroll + dx);
                    self.repeat = None;
                } else if matches!(touch.key, Key::Two { .. }) {
                    touch.doing = Doing::Flicking((dy / (measure.height - 10.0)).clamp(0.0, 1.0));
                }
            }
        }
        self.touch = Some(touch);
    }

    fn finger_up(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let Some(touch) = self.touch.take() else { return false };
        self.repeat = None;
        let layer = Self::layer(cx);
        match (touch.key, touch.doing) {
            (_, Doing::Scrolling(_)) => {}
            (Key::One { act: Act::Modifier(modifier), .. }, _) => armed::up(modifier, cx),
            (Key::One { act: Act::Press(press), .. }, _) if press.repeats() => {
                if !touch.sent {
                    Self::send(press, window, cx);
                }
            }
            (Key::One { act, .. }, _) => Self::act(act, window, cx),
            (Key::Two { top, below }, Doing::Flicking(progress)) => {
                Self::send(if progress < FLICK_AT { top.1 } else { below.1 }, window, cx);
            }
            (Key::Two { top, .. }, _) => Self::send(top.1, window, cx),
            // The way it points as the finger lifts goes once more (Blink's).
            (Key::Arrows, Doing::Steering(Some((arrow, _)))) => {
                Self::send(arrow.press(layer), window, cx);
            }
            (Key::Arrows, _) => {}
        }
        true
    }

    fn finger_cancelled(&mut self, cx: &mut Context<Self>) -> bool {
        self.repeat = None;
        let Some(touch) = self.touch.take() else { return false };
        if let Key::One { act: Act::Modifier(modifier), .. } = touch.key {
            armed::cancel(modifier, cx);
        }
        true
    }

    /// The joystick under a finger at `position`: the way it points now, its repeat started
    /// afresh when the way changes (Blink's), stopped in the dead zone.
    fn steer_at(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(mut touch), Some(bounds), Some(measure)) =
            (self.touch, self.bounds.get(), self.shown)
        else {
            return;
        };
        let Doing::Steering(was) = touch.doing else { return };
        let width = touch.key.width(measure);
        let plate = (width - 2.0 * PLATE_SIDE, measure.height - PLATE_TOP - PLATE_BOTTOM);
        let centre = Point {
            x: f32::from(bounds.origin.x) + touch.column + width / 2.0,
            y: f32::from(bounds.origin.y) + PLATE_TOP + plate.1 / 2.0,
        };
        let offset =
            Point { x: f32::from(position.x) - centre.x, y: f32::from(position.y) - centre.y };
        let now = steer(offset, plate);
        touch.doing = Doing::Steering(now);
        self.touch = Some(touch);
        let way = |steered: Option<(Arrow, Duration)>| steered.map(|(arrow, _)| arrow);
        if way(now) != way(was) {
            self.repeat = None;
            if now.is_some() {
                self.repeat_after(REPEAT_AFTER, window, cx);
            }
        }
    }

    /// After `wait`, send what the finger holds, then again at the pace it asks for, until it
    /// lifts or lets go of it.
    fn repeat_after(&mut self, wait: Duration, window: &mut Window, cx: &mut Context<Self>) {
        self.repeat = Some(cx.spawn_in(window, async move |row, cx| {
            let mut wait = wait;
            loop {
                cx.background_executor().timer(wait).await;
                match row.update_in(cx, |row, window, cx| row.repeat_once(window, cx)) {
                    Ok(Some(pace)) => wait = pace,
                    Ok(None) | Err(_) => break,
                }
            }
        }));
    }

    /// Send what the finger holds once more: its pace until the next, or `None` to stop.
    fn repeat_once(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<Duration> {
        let mut touch = self.touch?;
        let (press, pace) = match (touch.key, touch.doing) {
            (Key::One { act: Act::Press(press), .. }, Doing::Pressing) if press.repeats() => {
                (press, REPEAT_EVERY)
            }
            (Key::Arrows, Doing::Steering(Some((arrow, pace)))) => {
                (arrow.press(Self::layer(cx)), pace)
            }
            _ => return None,
        };
        touch.sent = true;
        self.touch = Some(touch);
        Self::send(press, window, cx);
        Some(pace)
    }
}

impl Render for KeyRow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.stands(window, cx) {
            // Away with the keyboard: what it armed goes off with it.
            if self.shown.take().is_some() {
                self.touch = None;
                self.repeat = None;
                self.scroll = 0.0;
                armed::clear(cx);
            }
            self.bounds.set(None);
            return div().into_any_element();
        }
        let viewport = window.viewport_size();
        let measure = measure(f32::from(viewport.width), f32::from(viewport.height));
        self.shown = Some(measure);
        let width = self
            .bounds
            .get()
            .map_or(f32::from(viewport.width), |bounds| f32::from(bounds.size.width));
        let laid = self.laid(width, measure, cx);
        self.scroll = self.scroll.clamp(0.0, laid.overflow);
        let now = armed::armed(cx);
        let layer = now.on(Modifier::Command);
        let theme = Theme::of(cx).clone();
        // Inked for the ground under it, as the session's band is.
        let ink = super::super::tabs::ink_for(self.ground, &theme);
        let touch = self.touch;
        let look = |placed: &Placed, column: f32| Look {
            measure,
            width: placed.width,
            armed: matches!(placed.key, Key::One { act: Act::Modifier(modifier), .. } if now.on(modifier)),
            pressed: touch.is_some_and(|touch| {
                touch.key == placed.key
                    && (touch.column - column).abs() < 0.5
                    && !matches!(touch.doing, Doing::Scrolling(_))
            }),
            flick: match touch {
                Some(Touch { key, doing: Doing::Flicking(progress), .. }) if key == placed.key => {
                    progress
                }
                _ => 0.0,
            },
            steering: match touch {
                Some(Touch { doing: Doing::Steering(Some((arrow, _))), .. }) => Some(arrow),
                _ => None,
            },
            layer,
        };
        let key =
            |placed: &Placed, column: f32, cx: &mut Context<Self>| -> gpui::Stateful<gpui::Div> {
                let pressed = placed.key;
                let drawn = draw_key(placed.key, &look(placed, column), &ink);
                if pressed == Key::Arrows {
                    // Its ways take their own presses.
                    drawn.child(arrow_presses(layer, cx))
                } else {
                    drawn.on_click(
                        cx.listener(move |_, _, window, cx| Self::clicked(pressed, window, cx)),
                    )
                }
            };
        let left: Vec<AnyElement> = laid
            .left
            .iter()
            .map(|placed| {
                key(placed, placed.x, cx).absolute().left(px(placed.x)).top_0().into_any_element()
            })
            .collect();
        // From the right edge, so they stand right whatever width the row was laid out for.
        let right_width: f32 = laid.right.iter().map(|placed| placed.width).sum();
        let mut from_right = right_width;
        let right: Vec<AnyElement> = laid
            .right
            .iter()
            .map(|placed| {
                from_right -= placed.width;
                key(placed, placed.x, cx)
                    .absolute()
                    .right(px(from_right))
                    .top_0()
                    .into_any_element()
            })
            .collect();
        let (strip_left, _) = laid.strip;
        let scroll = self.scroll;
        let keys: Vec<AnyElement> = laid
            .keys
            .iter()
            .map(|placed| {
                let column = placed.x + strip_left - scroll;
                key(placed, column, cx)
                    .absolute()
                    .left(px(placed.x - scroll))
                    .top_0()
                    .into_any_element()
            })
            .collect();
        // A hairline at each end of the strip while there is more of it that way (Blink's).
        let edge = ink.text.opacity(0.18);
        let hairline =
            |side: gpui::Div| side.absolute().top(px(10.0)).bottom(px(9.0)).w(px(1.0)).bg(edge);
        let strip = div()
            .id("key-row-strip")
            .debug_selector(|| "key-row-strip".into())
            .absolute()
            .left(px(strip_left))
            .right(px(right_width))
            .top_0()
            .h_full()
            .overflow_hidden()
            .children(keys)
            .when(scroll > 0.5, |strip| strip.child(hairline(div().left_0())))
            .when(scroll < laid.overflow - 0.5, |strip| strip.child(hairline(div().right_0())));
        let row = cx.weak_entity();
        let bounds = self.bounds.clone();
        let fingers = canvas(
            move |_, _, _| {},
            move |painted, (), window, _| {
                bounds.set(Some(painted));
                window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let ours =
                        row.update(cx, |row, cx| row.touched(event, window, cx)).unwrap_or(false);
                    if ours {
                        if event.phase == TouchPhase::Started {
                            window.prevent_default();
                        }
                        cx.stop_propagation();
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        div()
            .id("key-row")
            .debug_selector(|| "key-row".into())
            .role(gpui::Role::Toolbar)
            .aria_label("Keys")
            .relative()
            .flex_none()
            .w_full()
            .h(px(measure.height))
            .bg(self.ground)
            .children(left)
            .child(strip)
            .children(right)
            .child(fingers)
            .into_any_element()
    }
}

/// How one key is drawn this frame.
struct Look {
    measure: Measure,
    /// Its column.
    width: f32,
    /// A modifier that goes with the next key.
    armed: bool,
    /// Under the finger.
    pressed: bool,
    /// A two-symbol key's flick, 0 to 1.
    flick: f32,
    /// The way the joystick points.
    steering: Option<Arrow>,
    /// The ⌘ layer is on.
    layer: bool,
}

/// One key in its column: its plate, washed while pressed and solid while armed (as Blink lights
/// a modifier that is on), and its face; named for a screen reader.
fn draw_key(key: Key, look: &Look, ink: &Theme) -> gpui::Stateful<gpui::Div> {
    let (plate, glyph) = if look.armed {
        (ink.solid, ink.on_solid)
    } else if look.pressed {
        (ink.wash(0.26), ink.text)
    } else {
        (ink.wash(0.10), ink.text)
    };
    let face: AnyElement = match key {
        Key::One { face: Face::Text(words), .. } => div()
            .text_size(px(if words.is_ascii() { TEXT } else { SYMBOL }))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(glyph)
            .child(words)
            .into_any_element(),
        // The keyboard with a chevron under it, as the system's own hide key draws it.
        Key::One { face: Face::Icon(path), act: Act::HideKeyboard, .. } => div()
            .flex()
            .flex_col()
            .items_center()
            .child(icon(path).size(px(SYMBOL)).text_color(glyph))
            .child(icon(icons::ALT_ARROW_DOWN).size(px(9.0)).text_color(glyph))
            .into_any_element(),
        Key::One { face: Face::Icon(path), .. } => {
            icon(path).size(px(SYMBOL)).text_color(glyph).into_any_element()
        }
        Key::Two { top, below } => two_symbols(top.0, below.0, look, glyph),
        Key::Arrows => joystick(look, glyph),
    };
    let description: Option<SharedString> = match key {
        Key::Two { below, .. } => Some(format!("Flick down for {}", below.0).into()),
        Key::One { .. } | Key::Arrows => None,
    };
    let joystick = key == Key::Arrows;
    let name = if joystick && look.layer { "Home, End, Page Up and Page Down" } else { key.name() };
    let selector = format!("key-row-{}", key.name());
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .role(if joystick { gpui::Role::Group } else { gpui::Role::Button })
        .aria_label(name)
        .when_some(description, StatefulInteractiveElement::aria_description)
        .when(matches!(key, Key::One { act: Act::Modifier(_), .. }), |el| {
            el.aria_toggled(if look.armed { gpui::Toggled::True } else { gpui::Toggled::False })
        })
        .w(px(look.width))
        .h(px(look.measure.height))
        .pt(px(PLATE_TOP))
        .pb(px(PLATE_BOTTOM))
        .px(px(PLATE_SIDE))
        .child(
            div()
                .size_full()
                .rounded(px(PLATE_CORNER))
                .bg(plate)
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                .child(face),
        )
}

/// A two-symbol key's face: the second symbol small above the first, brought down to the middle
/// as a flick goes while the first sinks away (Blink's `KBKeyViewVertical2`).
fn two_symbols(top: &'static str, below: &'static str, look: &Look, glyph: Hsla) -> AnyElement {
    let flick = look.flick;
    let size = if top.len() > 1 { TEXT - 2.0 } else { SYMBOL };
    let travel = (look.measure.height - PLATE_TOP - PLATE_BOTTOM) * 0.3;
    let centred = |words: &'static str| {
        div()
            .absolute()
            .left_0()
            .right_0()
            .top_0()
            .bottom_0()
            .flex()
            .items_center()
            .justify_center()
            .child(words)
    };
    div()
        .relative()
        .size_full()
        .child(
            centred(below)
                .top(px(travel.mul_add(flick, -travel)))
                .text_size(px(size * 0.45_f32.mul_add(flick, 0.55)))
                .text_color(glyph.opacity(0.7_f32.mul_add(flick, 0.3))),
        )
        .child(
            centred(top)
                .top(px(14.0_f32.mul_add(flick, 1.0)))
                .text_size(px((size * (1.0 - flick)).max(0.01)))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(glyph.opacity((-0.5_f32).mul_add(flick, 1.0))),
        )
        .into_any_element()
}

/// The joystick's face: all four ways, the one steered at full strength (Blink's); under ⌘
/// Home, End, Page Up and Page Down in words.
fn joystick(look: &Look, glyph: Hsla) -> AnyElement {
    let tone = |arrow: Arrow| {
        let on = look.steering.is_none_or(|steered| steered == arrow);
        glyph.opacity(if on { 1.0 } else { 0.3 })
    };
    let way = |arrow: Arrow| -> AnyElement {
        if look.layer {
            let words = match arrow {
                Arrow::Left => "Home",
                Arrow::Down => "PgDn",
                Arrow::Up => "PgUp",
                Arrow::Right => "End",
            };
            div()
                .text_size(px(8.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(tone(arrow))
                .child(words)
                .into_any_element()
        } else {
            let path = match arrow {
                Arrow::Left => icons::ALT_ARROW_LEFT,
                Arrow::Down => icons::ALT_ARROW_DOWN,
                Arrow::Up => icons::ALT_ARROW_UP,
                Arrow::Right => icons::ALT_ARROW_RIGHT,
            };
            let size = if look.steering == Some(arrow) { 13.0 } else { 11.0 };
            icon(path).size(px(size)).text_color(tone(arrow)).into_any_element()
        }
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .child(way(Arrow::Up))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(if look.layer { 2.0 } else { 7.0 }))
                .child(way(Arrow::Left))
                .child(way(Arrow::Right)),
        )
        .child(way(Arrow::Down))
        .into_any_element()
}

/// The joystick's ways as presses of their own, for a pointer and a screen reader: the left and
/// right thirds, and the middle's top and bottom halves. A finger steers the whole key instead
/// (the row claims its touch, so these take none of it).
fn arrow_presses(layer: bool, cx: &mut Context<KeyRow>) -> AnyElement {
    let mut ways = Vec::with_capacity(Arrow::ALL.len());
    for arrow in Arrow::ALL {
        let name = arrow.name(layer);
        let way = div()
            .id(SharedString::from(format!("key-row-arrow-{name}")))
            .role(gpui::Role::Button)
            .aria_label(name)
            .absolute()
            .on_click(
                cx.listener(move |_, _, window, cx| KeyRow::send(arrow.press(layer), window, cx)),
            );
        let middle = |way: gpui::Stateful<gpui::Div>| {
            way.left(gpui::relative(0.3)).right(gpui::relative(0.3))
        };
        ways.push(match arrow {
            Arrow::Left => way.left_0().top_0().bottom_0().w(gpui::relative(0.3)),
            Arrow::Right => way.right_0().top_0().bottom_0().w(gpui::relative(0.3)),
            Arrow::Up => middle(way).top_0().h(gpui::relative(0.5)),
            Arrow::Down => middle(way).bottom_0().h(gpui::relative(0.5)),
        });
    }
    div().absolute().inset_0().children(ways).into_any_element()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{
        InputEvent as _, KeyDownEvent, TestAppContext, TouchEvent, TouchId, VisualTestContext,
    };

    use super::*;

    const PHONE: Measure = Measure { height: 44.0, key: 33.0, wide: 39.0 };

    /// Blink's measure: 44 pt in portrait with 33 pt keys (34 on the 6.7-inch phones), 38 in
    /// landscape with keys sized by the screen's width.
    #[test]
    fn the_row_takes_blinks_measure() {
        assert_eq!(measure(390.0, 844.0), PHONE, "a 6.1-inch phone upright");
        assert_eq!(
            measure(430.0, 932.0),
            Measure { height: 44.0, key: 34.0, wide: 40.0 },
            "6.7-inch"
        );
        assert_eq!(
            measure(844.0, 390.0),
            Measure { height: 38.0, key: 42.0, wide: 64.0 },
            "on its side"
        );
        assert_eq!(
            measure(667.0, 375.0),
            Measure { height: 38.0, key: 36.0, wide: 51.5 },
            "4.7-inch"
        );
        assert_eq!(
            measure(956.0, 440.0),
            Measure { height: 38.0, key: 47.0, wide: 68.0 },
            "the largest"
        );
        let (left, _) = sides(measure(844.0, 390.0));
        assert_eq!(left.first(), Some(&ESC_WIDE), "Esc widens on its side");
    }

    /// Esc ⌃ ⌥ at the left, the joystick, hide keyboard and ⌘ at the right, the strip between,
    /// its keys abutting and scrolling past its width.
    #[test]
    fn the_sides_stand_fixed_around_a_scrolling_strip() {
        let laid = lay_out(390.0, PHONE, &strip(running(Some(HarnessId::CLAUDE_CODE)), false));
        let names =
            |placed: &[Placed]| placed.iter().map(|placed| placed.key.name()).collect::<Vec<_>>();
        assert_eq!(names(&laid.left), ["Escape", "Control", "Option"], "the left");
        assert_eq!(names(&laid.right), ["Arrows", "Hide keyboard", "Command"], "the right");
        let left: Vec<_> = laid.left.iter().map(|placed| placed.x).collect();
        assert_eq!(left, [0.0, 33.0, 66.0], "abutting");
        let right: Vec<_> = laid.right.iter().map(|placed| (placed.x, placed.width)).collect();
        assert_eq!(right, [(279.0, 39.0), (318.0, 33.0), (351.0, 39.0)], "flush right");
        assert_eq!(laid.strip, (99.0, 180.0), "the strip between");
        let keys: f32 = laid.keys.iter().map(|placed| placed.width).sum();
        assert!(
            (laid.overflow - (keys - 180.0)).abs() < 0.01,
            "it scrolls by what runs past: {}",
            laid.overflow
        );
    }

    /// The strip leads with the keys of what runs, from the session's records, then the symbols,
    /// each once; under ⌘ it holds the function keys.
    #[test]
    fn the_strip_follows_what_runs() {
        let names = |keys: Vec<Key>| keys.iter().map(|key| key.name()).collect::<Vec<_>>();
        let claude = names(strip(running(Some(HarnessId::CLAUDE_CODE)), false));
        assert_eq!(
            claude.get(..5),
            Some(&["Shift Tab", "Control C", "/", "@", "Up"][..]),
            "Claude Code"
        );
        let shell = names(strip(running(None), false));
        assert_eq!(shell.get(..4), Some(&["Tab", "Control R", "Control C", "Up"][..]), "a shell");
        let codex = names(strip(running(Some(HarnessId::CODEX)), false));
        assert_eq!(codex.first(), Some(&"Control C"), "another agent, no mode key: {codex:?}");
        for keys in [&claude, &shell, &codex] {
            let mut seen = std::collections::HashSet::new();
            assert!(keys.iter().all(|key| seen.insert(*key)), "each key once: {keys:?}");
            assert!(!keys.contains(&"Escape"), "Esc stands fixed, never on the strip: {keys:?}");
            assert!(keys.contains(&"\\") && keys.contains(&"`"), "the symbols follow: {keys:?}");
        }
        let layer = names(strip(running(None), true));
        assert_eq!(layer, ["F1", "F2", "F3", "F4", "F5", "F6"], "the ⌘ layer");
    }

    /// A finger finds the key under it, on the strip as far as it is scrolled.
    #[test]
    fn a_finger_finds_its_key_as_the_strip_scrolls() {
        let laid = lay_out(390.0, PHONE, &strip(running(None), false));
        let name = |x: f32, scroll: f32| {
            key_at(&laid, x, scroll).map(|(placed, strip)| (placed.key.name(), strip))
        };
        assert_eq!(name(10.0, 0.0), Some(("Escape", false)), "the left");
        assert_eq!(name(380.0, 0.0), Some(("Command", false)), "the right");
        assert_eq!(name(100.0, 0.0), Some(("Tab", true)), "the strip's first");
        assert_eq!(name(100.0, 33.0), Some(("Control R", true)), "scrolled a key along");
        let placed = key_at(&laid, 100.0, 20.0).unwrap().0;
        assert!((placed.x - 79.0).abs() < 0.01, "its column moves with the strip: {placed:?}");
    }

    /// The joystick: nothing in its dead zone, the axis it leans along most, faster further out.
    #[test]
    fn the_joystick_steers_by_the_way_it_leans() {
        let plate = (37.0, 40.0);
        let at = |x: f32, y: f32| steer(Point { x, y }, plate);
        assert_eq!(at(5.0, 4.0), None, "the dead zone");
        assert_eq!(at(-20.0, 3.0), Some((Arrow::Left, REPEAT_EVERY)));
        assert_eq!(at(8.0, -30.0), Some((Arrow::Up, REPEAT_EVERY)));
        assert_eq!(at(40.0, 0.0), Some((Arrow::Right, FASTER)), "past 0.9 plates");
        assert_eq!(at(0.0, 170.0), Some((Arrow::Down, FASTEST)), "past four plates down");
        assert_eq!(Arrow::Up.press(true), Press::key("pageup"), "under ⌘");
    }

    /// The window the row stands in, over a stand-in terminal that spends the armed modifiers as
    /// the terminal does and writes down each key as it would go.
    struct Host {
        row: gpui::Entity<KeyRow>,
        terminal: FocusHandle,
        sent: Rc<RefCell<Vec<String>>>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let sent = self.sent.clone();
            div()
                .size_full()
                .flex()
                .flex_col()
                .track_focus(&self.terminal)
                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                    let spent = armed::spend(cx);
                    let modifiers = event.keystroke.modifiers;
                    let mut said = String::new();
                    for (on, word) in [
                        (modifiers.control || spent.control, "ctrl-"),
                        (modifiers.alt || spent.alt, "alt-"),
                        (modifiers.shift, "shift-"),
                        (modifiers.platform || spent.command, "cmd-"),
                    ] {
                        if on {
                            said.push_str(word);
                        }
                    }
                    said.push_str(&event.keystroke.key);
                    sent.borrow_mut().push(said);
                })
                .child(div().flex_1())
                .child(self.row.clone())
        }
    }

    /// A phone upright with the soft keyboard up over a Claude Code session's terminal.
    fn row(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<KeyRow>, Rc<RefCell<Vec<String>>>, &mut VisualTestContext) {
        FORCE_KEYBOARD.with(|force| force.set(true));
        cx.update(|cx| cx.set_global(Theme::default()));
        let sent = Rc::new(RefCell::new(Vec::new()));
        let kept = sent.clone();
        let (host, cx) = cx.add_window_view(move |_, cx| Host {
            row: cx.new(KeyRow::new),
            terminal: cx.focus_handle(),
            sent: kept,
        });
        cx.simulate_resize(gpui::size(px(390.0), px(844.0)));
        let (row, terminal) =
            host.read_with(cx, |host, _| (host.row.clone(), host.terminal.clone()));
        cx.update(|window, cx| {
            window.focus(&terminal, cx);
            row.update(cx, |row, cx| {
                row.set(running(Some(HarnessId::CLAUDE_CODE)), gpui::black(), terminal.clone(), cx);
            });
        });
        draw(cx);
        (row, sent, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        }
    }

    fn finger(cx: &mut VisualTestContext, phase: TouchPhase, at: Point<Pixels>) {
        cx.update(|window, cx| {
            let event = TouchEvent {
                id: TouchId(1),
                phase,
                position: at,
                predicted_position: None,
                force: None,
                timestamp: None,
            };
            window.dispatch_event(event.to_platform_input(), cx);
        });
        draw(cx);
    }

    fn tap(cx: &mut VisualTestContext, selector: &'static str) {
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
        finger(cx, TouchPhase::Started, at);
        finger(cx, TouchPhase::Ended, at);
    }

    fn sent(sent: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
        sent.borrow_mut().drain(..).collect()
    }

    /// A tap sends its key, as a keyboard's would; a modifier tapped goes with the next key and
    /// then goes off, and a second tap puts it off with no key spent.
    #[gpui::test]
    fn a_tap_sends_its_key_and_a_tapped_modifier_goes_with_the_next(cx: &mut TestAppContext) {
        let (_row, keys, cx) = row(cx);
        assert!(cx.debug_bounds("key-row").is_some(), "the row stands over the keyboard");
        tap(cx, "key-row-Shift Tab");
        tap(cx, "key-row-Control C");
        assert_eq!(sent(&keys), ["shift-tab", "ctrl-c"], "Claude Code's keys, as typed");
        tap(cx, "key-row-Control");
        let latched = cx.update(|_, cx| armed::armed(cx).arm(Modifier::Control));
        assert_eq!(latched, armed::Arm::Latched, "armed for the next key");
        tap(cx, "key-row-/");
        tap(cx, "key-row-/");
        assert_eq!(sent(&keys), ["ctrl-/", "/"], "the next key takes it, the one after goes plain");
        tap(cx, "key-row-Option");
        tap(cx, "key-row-Option");
        assert!(!cx.update(|_, cx| armed::armed(cx).on(Modifier::Alt)), "a second tap puts it off");
        assert!(sent(&keys).is_empty(), "a modifier sends nothing of its own");
    }

    /// Held, a modifier goes with every key the system keyboard types until it lifts, and then
    /// nothing is left armed.
    #[gpui::test]
    fn a_held_modifier_chords_until_it_lifts(cx: &mut TestAppContext) {
        let (_row, keys, cx) = row(cx);
        let control = cx.debug_bounds("key-row-Control").expect("⌃").center();
        finger(cx, TouchPhase::Started, control);
        for key in ["c", "d"] {
            cx.update(|window, cx| {
                window
                    .dispatch_keystroke(Keystroke { key: key.into(), ..Keystroke::default() }, cx);
            });
        }
        finger(cx, TouchPhase::Ended, control);
        assert_eq!(sent(&keys), ["ctrl-c", "ctrl-d"], "each key while held");
        assert!(
            !cx.update(|_, cx| armed::armed(cx).on(Modifier::Control)),
            "a chord leaves nothing armed"
        );
    }

    /// A flick down a two-symbol key past three quarters of its travel gives the second symbol;
    /// a short one the first.
    #[gpui::test]
    fn a_flick_down_gives_the_second_symbol(cx: &mut TestAppContext) {
        let (_row, keys, cx) = row(cx);
        let at = cx.debug_bounds("key-row-@").expect("@").center();
        for (travel, said) in [(30.0, "#"), (10.0, "@")] {
            let to = gpui::point(at.x, at.y + px(travel));
            finger(cx, TouchPhase::Started, at);
            finger(cx, TouchPhase::Moved, to);
            finger(cx, TouchPhase::Ended, to);
            assert_eq!(sent(&keys), [said], "a flick of {travel} pt");
        }
    }

    /// Sideways on the strip a finger scrolls it, past the slop, and the key it came down on goes
    /// unsent, even one that repeats.
    #[gpui::test]
    fn a_sideways_finger_scrolls_the_strip_and_sends_nothing(cx: &mut TestAppContext) {
        let (row, keys, cx) = row(cx);
        let at = cx.debug_bounds("key-row-Shift Tab").expect("⇧⇥").center();
        finger(cx, TouchPhase::Started, at);
        finger(cx, TouchPhase::Moved, gpui::point(at.x - px(20.0), at.y));
        finger(cx, TouchPhase::Moved, gpui::point(at.x - px(80.0), at.y));
        finger(cx, TouchPhase::Ended, gpui::point(at.x - px(80.0), at.y));
        assert!(sent(&keys).is_empty(), "nothing sent");
        let scroll = row.read_with(cx, |row, _| row.scroll);
        assert!(
            (scroll - 60.0).abs() < 0.5,
            "the strip followed the finger past the slop: {scroll}"
        );
    }

    /// A held Esc goes as the finger comes down, again after half a second, then every tenth;
    /// the joystick sends the way it is pressed toward, repeats held there, and once more as it
    /// lifts.
    #[gpui::test]
    fn held_keys_repeat_and_the_joystick_steers(cx: &mut TestAppContext) {
        let (_row, keys, cx) = row(cx);
        let esc = cx.debug_bounds("key-row-Escape").expect("Esc").center();
        finger(cx, TouchPhase::Started, esc);
        assert_eq!(sent(&keys), ["escape"], "as it comes down");
        cx.executor().advance_clock(REPEAT_AFTER);
        draw(cx);
        cx.executor().advance_clock(REPEAT_EVERY);
        draw(cx);
        finger(cx, TouchPhase::Ended, esc);
        cx.executor().advance_clock(REPEAT_AFTER);
        draw(cx);
        assert_eq!(sent(&keys), ["escape", "escape"], "two repeats, none after it lifts");
        let arrows = cx.debug_bounds("key-row-Arrows").expect("the joystick");
        let left = gpui::point(arrows.origin.x + px(4.0), arrows.center().y);
        finger(cx, TouchPhase::Started, left);
        assert!(sent(&keys).is_empty(), "a way goes as it lifts, or held");
        cx.executor().advance_clock(REPEAT_AFTER);
        draw(cx);
        finger(cx, TouchPhase::Ended, left);
        assert_eq!(sent(&keys), ["left", "left"], "repeated, then once more as it lifts");
        finger(cx, TouchPhase::Started, arrows.center());
        finger(cx, TouchPhase::Ended, arrows.center());
        assert!(sent(&keys).is_empty(), "the dead zone sends nothing");
    }

    /// Under ⌘ the strip holds the function keys, and the ⌘ goes off with the key it went with.
    #[gpui::test]
    fn the_command_layer_holds_the_function_keys(cx: &mut TestAppContext) {
        let (_row, keys, cx) = row(cx);
        tap(cx, "key-row-Command");
        assert!(cx.debug_bounds("key-row-F1").is_some(), "the layer's strip");
        assert!(cx.debug_bounds("key-row-@").is_none(), "the symbols give way");
        tap(cx, "key-row-F1");
        assert_eq!(sent(&keys), ["cmd-f1"], "the key, the ⌘ spent with it");
        assert!(cx.debug_bounds("key-row-@").is_some(), "the layer goes with the ⌘");
    }

    /// An iPad's shortcut bar holds Esc and the modifiers ahead, the keys of what runs and the
    /// arrows after, as taps: a modifier arms for the next key, and a key goes into the window
    /// holding the keyboard as a hardware key does. A key the bar no longer holds does nothing.
    #[gpui::test]
    fn an_ipads_shortcut_bar_holds_the_rows_keys_as_taps(cx: &mut TestAppContext) {
        let (_row, keys, cx) = row(cx);
        let faces = |keys: &[armed::BarKey]| keys.iter().map(|key| key.face).collect::<Vec<_>>();
        let names = |keys: &[armed::BarKey]| keys.iter().map(|key| key.name).collect::<Vec<_>>();
        let claude = bar(Some(HarnessId::CLAUDE_CODE));
        assert_eq!(faces(&claude.leading), ["esc", "\u{2303}", "\u{2325}", "\u{2318}"], "ahead");
        assert_eq!(
            names(&claude.trailing),
            ["Shift Tab", "Control C", "Left", "Down", "Up", "Right"],
            "Claude Code's one-act keys, then the arrows: the symbols stay on the keyboard"
        );
        let shell = names(&bar(None).trailing);
        assert_eq!(shell.get(..3), Some(&["Tab", "Control R", "Control C"][..]), "a shell's");
        let heard = Rc::new(Cell::new(0));
        let counted = heard.clone();
        let _watch =
            cx.update(|_, cx| armed::observe_bar(cx, move |_| counted.set(counted.get() + 1)));
        cx.update(|_, cx| {
            armed::set_bar(claude.clone(), cx);
            armed::set_bar(claude.clone(), cx);
        });
        assert_eq!(heard.get(), 1, "only a change is heard");
        cx.cx.update(|cx| armed::press_bar(true, 1, cx));
        let latched = cx.update(|_, cx| armed::armed(cx).arm(Modifier::Control));
        assert_eq!(latched, armed::Arm::Latched, "a tapped ⌃ arms for the next key");
        cx.cx.update(|cx| {
            armed::press_bar(false, 0, cx);
            armed::press_bar(false, 4, cx);
            armed::press_bar(false, 99, cx);
        });
        draw(cx);
        assert_eq!(sent(&keys), ["ctrl-shift-tab", "up"], "the ⌃ goes with the next key only");
        cx.update(|_, cx| armed::set_bar(armed::Bar::default(), cx));
        cx.cx.update(|cx| armed::press_bar(false, 0, cx));
        draw(cx);
        assert!(sent(&keys).is_empty(), "an empty bar holds nothing to press");
    }

    /// The row stands only while the soft keyboard is up over the terminal: a hardware keyboard
    /// or the keyboard elsewhere takes it away, with whatever it armed.
    #[gpui::test]
    fn the_row_goes_with_a_hardware_keyboard_or_the_terminals_focus(cx: &mut TestAppContext) {
        let (row, _keys, cx) = row(cx);
        tap(cx, "key-row-Control");
        cx.update(|_, cx| armed::set_hardware_keyboard(true, cx));
        draw(cx);
        assert!(cx.debug_bounds("key-row").is_none(), "a hardware keyboard stands in for it");
        assert!(
            !cx.update(|_, cx| armed::armed(cx).on(Modifier::Control)),
            "what it armed goes off"
        );
        assert_eq!(row.read_with(cx, |row, _| row.height()), 0.0, "it takes no room");
        cx.update(|_, cx| armed::set_hardware_keyboard(false, cx));
        draw(cx);
        assert_eq!(row.read_with(cx, |row, _| row.height()), 44.0, "back with the soft keyboard");
        cx.update(|window, cx| window.focus(&cx.focus_handle(), cx));
        draw(cx);
        assert!(cx.debug_bounds("key-row").is_none(), "the keyboard is not the terminal's");
    }
}
