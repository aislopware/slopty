//! The touch screen's key row, as Slopty's own views meet it.
//!
//! That is the modifiers it arms for the keys that follow, and whether a hardware keyboard stands
//! in for it (`docs/decisions/layout.md`, "iPhone is a list, then the terminal").
//!
//! The modifiers behave as iPadOS's own sticky modifiers and Blink Shell's key row do: a tap arms
//! one for the next key, a second tap puts it off, and one held under a finger goes with every key
//! until it lifts. The row arms them and draws them; the terminal spends them, since the keys they
//! go with come from the row and from the system keyboard alike, and only the terminal hears the
//! second. So they stand where both reach, an app [`Global`], and the row is drawn again whenever
//! the terminal spends one ([`observe`]).
//!
//! The row hides while a hardware keyboard is attached, which the app hears from the platform and
//! says here ([`set_hardware_keyboard`]).
//!
//! An iPad, laid out as the Mac, has no row: its keys sit in the system keyboard's own shortcut
//! bar, which only the app's platform layer can fill. The shell says here what the bar holds
//! ([`set_bar`]) and the platform layer draws it and passes its taps back ([`press_bar`]).

use gpui::{App, Global, KeyUpEvent, Keystroke, PlatformInput, Subscription};

/// A modifier the key row arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// ⌃: the next key as a control key.
    Control,
    /// ⌥: the next key as Meta (`ESC` and the key), whatever the ⌥ setting.
    Alt,
    /// ⌘: the next typed character as the app's ⌘ shortcut, the next ⌫ ← → as the Mac's line
    /// editing, the next tap on a link opening it.
    Command,
}

/// How one modifier stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arm {
    /// Off.
    #[default]
    Off,
    /// Tapped: on for the next key, then off.
    Latched,
    /// Under a finger: on for every key until the finger lifts.
    Held,
}

/// One modifier's state, with what its finger did while it held it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Slot {
    arm: Arm,
    /// A key went with it while it was held: lifting the finger puts it off.
    used: bool,
    /// It was latched when the finger came down: lifting the finger puts it off (the second
    /// tap).
    was_latched: bool,
}

impl Slot {
    const fn down(&mut self) {
        *self = Self { arm: Arm::Held, used: false, was_latched: matches!(self.arm, Arm::Latched) };
    }

    const fn up(&mut self) {
        if !matches!(self.arm, Arm::Held) {
            return;
        }
        let off = self.used || self.was_latched;
        *self = Self {
            arm: if off { Arm::Off } else { Arm::Latched },
            used: false,
            was_latched: false,
        };
    }

    const fn spend(&mut self) -> bool {
        match self.arm {
            Arm::Off => false,
            Arm::Latched => {
                self.arm = Arm::Off;
                true
            }
            Arm::Held => {
                self.used = true;
                true
            }
        }
    }
}

/// Which modifiers went with a key ([`Armed::spend`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// ⌃ went with it.
    pub control: bool,
    /// ⌥ went with it.
    pub alt: bool,
    /// ⌘ went with it.
    pub command: bool,
}

/// The three modifiers the key row arms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Armed {
    control: Slot,
    alt: Slot,
    command: Slot,
}

impl Armed {
    const fn slot(&self, modifier: Modifier) -> &Slot {
        match modifier {
            Modifier::Control => &self.control,
            Modifier::Alt => &self.alt,
            Modifier::Command => &self.command,
        }
    }

    const fn slot_mut(&mut self, modifier: Modifier) -> &mut Slot {
        match modifier {
            Modifier::Control => &mut self.control,
            Modifier::Alt => &mut self.alt,
            Modifier::Command => &mut self.command,
        }
    }

    /// A finger came down on `modifier`'s key: it is held.
    pub const fn down(&mut self, modifier: Modifier) {
        self.slot_mut(modifier).down();
    }

    /// The finger lifted from `modifier`'s key: a tap with no key under it arms it for the next
    /// key, and a tap on it armed, or a hold that went with a key, puts it off.
    pub const fn up(&mut self, modifier: Modifier) {
        self.slot_mut(modifier).up();
    }

    /// The system took the finger on `modifier`'s key: it goes off, as if never pressed.
    pub fn cancel(&mut self, modifier: Modifier) {
        *self.slot_mut(modifier) = Slot::default();
    }

    /// A key went: which modifiers go with it. Each latched one is spent, and each held one
    /// stays for the next key.
    pub const fn spend(&mut self) -> Spent {
        Spent {
            control: self.control.spend(),
            alt: self.alt.spend(),
            command: self.command.spend(),
        }
    }

    /// Spend `modifier` alone, for what only it means (⌘ on a tap opens the link under it):
    /// whether it was on.
    pub const fn spend_one(&mut self, modifier: Modifier) -> bool {
        self.slot_mut(modifier).spend()
    }

    /// How `modifier` stands.
    #[must_use]
    pub const fn arm(&self, modifier: Modifier) -> Arm {
        self.slot(modifier).arm
    }

    /// Whether `modifier` goes with the next key.
    #[must_use]
    pub const fn on(&self, modifier: Modifier) -> bool {
        !matches!(self.slot(modifier).arm, Arm::Off)
    }
}

/// The modifiers the key row armed, for the whole app: one keyboard types at a time.
#[derive(Default)]
struct Shared(Armed);

impl Global for Shared {}

/// How the key row's modifiers stand now.
#[must_use]
pub fn armed(cx: &App) -> Armed {
    cx.try_global::<Shared>().map(|shared| shared.0).unwrap_or_default()
}

/// Change the modifiers by `change`, telling the observers only when they changed.
fn change<R>(cx: &mut App, change: impl FnOnce(&mut Armed) -> R) -> R {
    let was = armed(cx);
    let mut now = was;
    let said = change(&mut now);
    if now != was {
        cx.set_global(Shared(now));
    }
    said
}

/// A finger came down on `modifier`'s key ([`Armed::down`]).
pub fn down(modifier: Modifier, cx: &mut App) {
    change(cx, |armed| armed.down(modifier));
}

/// The finger lifted from `modifier`'s key ([`Armed::up`]).
pub fn up(modifier: Modifier, cx: &mut App) {
    change(cx, |armed| armed.up(modifier));
}

/// The system took the finger on `modifier`'s key ([`Armed::cancel`]).
pub fn cancel(modifier: Modifier, cx: &mut App) {
    change(cx, |armed| armed.cancel(modifier));
}

/// Every modifier off: the row went away with the keyboard.
pub fn clear(cx: &mut App) {
    put_back(Armed::default(), cx);
}

/// The modifiers as `kept` had them: keys a view types on its own (a click's arrows to the
/// cursor) leave the person's armed ones for the person's next key.
pub fn put_back(kept: Armed, cx: &mut App) {
    change(cx, |armed| *armed = kept);
}

/// A key went into a terminal: which modifiers go with it ([`Armed::spend`]).
pub fn spend(cx: &mut App) -> Spent {
    change(cx, Armed::spend)
}

/// Spend `modifier` alone ([`Armed::spend_one`]).
pub fn spend_one(modifier: Modifier, cx: &mut App) -> bool {
    change(cx, |armed| armed.spend_one(modifier))
}

/// Call `changed` whenever the modifiers change: the row draws them again.
pub fn observe(cx: &mut App, mut changed: impl FnMut(&mut App) + 'static) -> Subscription {
    cx.observe_global::<Shared>(move |cx| changed(cx))
}

/// Whether a hardware keyboard is attached, as the app last heard.
#[derive(Default)]
struct HardwareKeyboard(bool);

impl Global for HardwareKeyboard {}

/// Whether a hardware keyboard is attached: the row hides while one is. `false` until the app
/// says, and always on a build that never does.
#[must_use]
pub fn hardware_keyboard(cx: &App) -> bool {
    cx.try_global::<HardwareKeyboard>().is_some_and(|keyboard| keyboard.0)
}

/// The app heard a hardware keyboard come (`true`) or go: the row hides or shows again, and the
/// modifiers it armed go off.
pub fn set_hardware_keyboard(attached: bool, cx: &mut App) {
    if hardware_keyboard(cx) != attached {
        cx.set_global(HardwareKeyboard(attached));
        clear(cx);
    }
}

/// Call `changed` whenever a hardware keyboard comes or goes.
pub fn observe_hardware_keyboard(
    cx: &mut App,
    mut changed: impl FnMut(&mut App) + 'static,
) -> Subscription {
    cx.observe_global::<HardwareKeyboard>(move |cx| changed(cx))
}

/// A key in an iPad keyboard's shortcut bar ([`Bar`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarKey {
    /// What it shows: a glyph, or a word as short as one.
    pub face: &'static str,
    /// What a screen reader calls it.
    pub name: &'static str,
    /// What a tap on it does.
    pub press: BarPress,
}

/// What a tap on a shortcut bar's key does. The bar takes taps only, so a modifier there arms
/// for the next key and never holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BarPress {
    /// The key goes into the window as a keyboard's key down and up.
    Key(Keystroke),
    /// The modifier arms for the next key, or a second tap puts it off.
    Modifier(Modifier),
}

/// What an iPad keyboard's shortcut bar holds: keys before the system's own, and keys after.
/// Empty, the bar holds only the system's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bar {
    /// The keys at the bar's leading edge.
    pub leading: Vec<BarKey>,
    /// The keys at its trailing edge.
    pub trailing: Vec<BarKey>,
}

/// What the shortcut bar holds now, for the whole app.
#[derive(Default)]
struct SharedBar(Bar);

impl Global for SharedBar {}

/// What the shortcut bar holds now.
#[must_use]
pub fn bar(cx: &App) -> Bar {
    cx.try_global::<SharedBar>().map(|shared| shared.0.clone()).unwrap_or_default()
}

/// The shell says what the shortcut bar holds; its observers hear only a change.
pub fn set_bar(bar: Bar, cx: &mut App) {
    let same = cx
        .try_global::<SharedBar>()
        .map_or_else(|| bar == Bar::default(), |shared| shared.0 == bar);
    if !same {
        cx.set_global(SharedBar(bar));
    }
}

/// Call `changed` whenever what the shortcut bar holds changes: the platform layer draws it
/// again.
pub fn observe_bar(cx: &mut App, mut changed: impl FnMut(&mut App) + 'static) -> Subscription {
    cx.observe_global::<SharedBar>(move |cx| changed(cx))
}

/// The person tapped the shortcut bar's `index`th key on its leading edge, or its trailing one.
///
/// A modifier is tapped, and a key goes into the window holding the keyboard, which takes it as
/// a hardware key. A key the bar no longer holds does nothing.
pub fn press_bar(leading: bool, index: usize, cx: &mut App) {
    let bar = bar(cx);
    let side = if leading { &bar.leading } else { &bar.trailing };
    let Some(key) = side.get(index) else {
        return;
    };
    match &key.press {
        BarPress::Modifier(modifier) => {
            down(*modifier, cx);
            up(*modifier, cx);
        }
        BarPress::Key(keystroke) => {
            let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
                return;
            };
            let keystroke = keystroke.clone();
            // A window that closed in between takes no key.
            let _closed = window.update(cx, |_, window, cx| {
                window.dispatch_keystroke(keystroke.clone(), cx);
                window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Arm, Armed, Modifier, Spent};

    const CONTROL: Modifier = Modifier::Control;

    /// A tap arms the modifier for one key; the key spends it.
    #[test]
    fn a_tap_arms_for_the_next_key() {
        let mut armed = Armed::default();
        armed.down(CONTROL);
        assert_eq!(armed.arm(CONTROL), Arm::Held, "lit while the finger is on it");
        armed.up(CONTROL);
        assert_eq!(armed.arm(CONTROL), Arm::Latched, "armed for the next key");
        assert_eq!(armed.spend(), Spent { control: true, ..Spent::default() }, "the key takes it");
        assert_eq!(armed.arm(CONTROL), Arm::Off, "spent");
        assert_eq!(armed.spend(), Spent::default(), "the key after goes plain");
    }

    /// A second tap on an armed modifier puts it off, with no key spent.
    #[test]
    fn a_second_tap_puts_it_off() {
        let mut armed = Armed::default();
        armed.down(CONTROL);
        armed.up(CONTROL);
        armed.down(CONTROL);
        assert!(armed.on(CONTROL), "still on under the finger");
        armed.up(CONTROL);
        assert_eq!(armed.arm(CONTROL), Arm::Off, "the second tap");
    }

    /// Held, it goes with every key, and lifting it after a key puts it off.
    #[test]
    fn held_it_chords_every_key_until_it_lifts() {
        let mut armed = Armed::default();
        armed.down(CONTROL);
        for _ in 0..3 {
            assert!(armed.spend().control, "each key while held");
        }
        armed.up(CONTROL);
        assert_eq!(armed.arm(CONTROL), Arm::Off, "a chord leaves nothing armed");
    }

    /// A hold that began on a latched modifier and went with a key is off when it lifts.
    #[test]
    fn a_latched_modifier_held_through_a_key_lifts_off() {
        let mut armed = Armed::default();
        armed.down(CONTROL);
        armed.up(CONTROL);
        armed.down(CONTROL);
        assert!(armed.spend().control, "held through a key");
        armed.up(CONTROL);
        assert_eq!(armed.arm(CONTROL), Arm::Off, "off when it lifts");
    }

    /// The modifiers stand apart, and a key spends every latched one at once.
    #[test]
    fn a_key_spends_every_latched_modifier_and_keeps_the_held() {
        let mut armed = Armed::default();
        armed.down(Modifier::Alt);
        armed.up(Modifier::Alt);
        armed.down(Modifier::Command);
        armed.up(Modifier::Command);
        armed.down(CONTROL);
        assert_eq!(
            armed.spend(),
            Spent { control: true, alt: true, command: true },
            "all three go"
        );
        assert!(!armed.on(Modifier::Alt) && !armed.on(Modifier::Command), "the latched are spent");
        assert!(armed.on(CONTROL), "the held stays");
        assert!(armed.spend_one(CONTROL), "spent alone");
        assert!(!armed.spend_one(Modifier::Alt), "an off modifier spends nothing");
    }

    /// A cancelled touch leaves the modifier off; a lift with no touch first changes nothing.
    #[test]
    fn a_cancelled_touch_leaves_it_off() {
        let mut armed = Armed::default();
        armed.down(CONTROL);
        armed.cancel(CONTROL);
        assert_eq!(armed, Armed::default(), "cancelled");
        armed.up(CONTROL);
        assert_eq!(armed, Armed::default(), "an unmatched lift");
        armed.down(CONTROL);
        armed.up(CONTROL);
        armed.up(CONTROL);
        assert_eq!(armed.arm(CONTROL), Arm::Latched, "a second unmatched lift keeps it latched");
    }
}
