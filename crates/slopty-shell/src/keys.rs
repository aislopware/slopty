//! slopty: how a key reads wherever Slopty names one: a tooltip, a menu row, the palette, the
//! shortcuts page and the accessibility tree (`docs/decisions/ui.md`, "Tooltips name the action
//! and its key").
//!
//! On the Mac a stroke is its modifier glyphs in ⌃⌥⇧⌘ order with no separators, then the key
//! ("⇧⌘A"), with Claude desktop's key names (⏎ ⌫ ⇥ ␣ ← → ↑ ↓ and the word "Esc"); elsewhere it is
//! words ("Ctrl+Shift+A"). The binding shown is the one the keymap holds when the hint draws, so
//! a rebind shows at once and an unbound action shows nothing.

use gpui::{Action, App, FocusHandle, KeyBinding, Modifiers, SharedString, Window};

/// Between the chords of a multi-stroke binding ("⌘K ⌘S"): a thin space.
const CHORD_GAP: char = '\u{2009}';

/// How a key reads in Slopty's chrome: Mac glyphs in ⌃⌥⇧⌘ order with no separators ("⇧⌘A"),
/// Claude desktop's key names (⏎ ⌫ ⇥ ␣ ← → ↑ ↓, "Esc"); elsewhere "Ctrl+Shift+A".
pub fn stroke_text(modifiers: &Modifiers, key: &str) -> String {
    stroke_text_on(cfg!(target_os = "macos"), modifiers, key)
}

/// [`stroke_text`] for an explicit platform, so both spellings are testable on any machine.
pub fn stroke_text_on(mac: bool, modifiers: &Modifiers, key: &str) -> String {
    if mac {
        let mut out = String::new();
        for (on, glyph) in [
            (modifiers.control, '⌃'),
            (modifiers.alt, '⌥'),
            (modifiers.shift, '⇧'),
            (modifiers.platform, '⌘'),
        ] {
            if on {
                out.push(glyph);
            }
        }
        out.push_str(&key_name(true, key));
        return out;
    }
    let mut parts: Vec<String> = Vec::new();
    for (on, word) in [
        (modifiers.control, "Ctrl"),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
        (modifiers.platform, "Super"),
    ] {
        if on {
            parts.push(word.to_owned());
        }
    }
    parts.push(key_name(false, key));
    parts.join("+")
}

/// One key's name: the Mac's glyph, or its word elsewhere. A letter reads in capitals, a
/// function key as "F5", anything else as itself.
fn key_name(mac: bool, key: &str) -> String {
    let named = match (key, mac) {
        ("enter", true) => "⏎",
        ("enter", false) => "Enter",
        ("escape", _) => "Esc",
        ("backspace", true) => "⌫",
        ("backspace", false) => "Backspace",
        ("delete", true) => "⌦",
        ("delete", false) => "Delete",
        ("tab", true) => "⇥",
        ("tab", false) => "Tab",
        ("space", true) => "␣",
        ("space", false) => "Space",
        ("up", true) => "↑",
        ("down", true) => "↓",
        ("left", true) => "←",
        ("right", true) => "→",
        ("up", false) => "Up",
        ("down", false) => "Down",
        ("left", false) => "Left",
        ("right", false) => "Right",
        ("home", true) => "↖",
        ("end", true) => "↘",
        ("pageup", true) => "⇞",
        ("pagedown", true) => "⇟",
        ("home", false) => "Home",
        ("end", false) => "End",
        ("pageup", false) => "PgUp",
        ("pagedown", false) => "PgDn",
        _ => return key.to_uppercase(),
    };
    named.to_owned()
}

/// A binding's chords, separated by a thin space ("⌘K ⌘S").
pub fn binding_text(binding: &KeyBinding) -> SharedString {
    let mut out = String::new();
    for (n, stroke) in binding.keystrokes().iter().enumerate() {
        if n > 0 {
            out.push(CHORD_GAP);
        }
        out.push_str(&stroke_text(stroke.modifiers(), stroke.key()));
    }
    out.into()
}

/// The binding `action` would fire on now: in `focus`'s key context when given, else the focused
/// element's, else the window's (Zed's `KeyBinding::for_action_in` order), else any binding the
/// keymap holds with no context. `None` when nothing binds it.
pub fn binding_for_action(
    action: &dyn Action,
    focus: Option<&FocusHandle>,
    window: &Window,
    cx: &App,
) -> Option<KeyBinding> {
    focus
        .cloned()
        .or_else(|| window.focused(cx))
        .and_then(|focus| window.highest_precedence_binding_for_action_in(action, &focus))
        .or_else(|| window.highest_precedence_binding_for_action(action))
        .or_else(|| global_binding(action, cx))
}

/// [`binding_for_action`] as [`binding_text`] writes it.
pub fn for_action(
    action: &dyn Action,
    focus: Option<&FocusHandle>,
    window: &Window,
    cx: &App,
) -> Option<SharedString> {
    binding_for_action(action, focus, window, cx).map(|binding| binding_text(&binding))
}

/// The last context-free binding of `action` (the keymap's precedence: later wins), for when no
/// frame has resolved one.
fn global_binding(action: &dyn Action, cx: &App) -> Option<KeyBinding> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap
        .bindings_for_action(action)
        .filter(|binding| binding.predicate().is_none())
        .last()
        .cloned()
}

/// A stored combo ("mod-shift-a", "shift-tab", "escape") as [`stroke_text`] writes it, for keys a
/// raw handler takes and no action carries, and for the shortcuts the settings store.
pub fn combo_text(combo: &str) -> SharedString {
    combo_text_on(cfg!(target_os = "macos"), combo).into()
}

/// [`combo_text`] for an explicit platform.
pub fn combo_text_on(mac: bool, combo: &str) -> String {
    combo
        .split_whitespace()
        .map(|chord| {
            let (modifiers, key) = parse_chord(mac, chord);
            stroke_text_on(mac, &modifiers, key)
        })
        .collect::<Vec<_>>()
        .join(&CHORD_GAP.to_string())
}

/// One stored chord's modifiers and key. "mod" is ⌘ on the Mac and Ctrl elsewhere; a trailing
/// "-" after a separator is the minus key ("mod--").
fn parse_chord(mac: bool, chord: &str) -> (Modifiers, &str) {
    let (prefix, key) = match chord.strip_suffix("--") {
        Some(prefix) => (prefix, "-"),
        None => match chord.rsplit_once('-') {
            Some((prefix, key)) => (prefix, key),
            None => ("", chord),
        },
    };
    let mut modifiers = Modifiers::default();
    for part in prefix.split('-').filter(|part| !part.is_empty()) {
        match part {
            "mod" if mac => modifiers.platform = true,
            "mod" => modifiers.control = true,
            "cmd" | "super" | "win" | "platform" => modifiers.platform = true,
            "ctrl" | "control" => modifiers.control = true,
            "alt" | "opt" | "option" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "fn" | "function" => modifiers.function = true,
            _ => {}
        }
    }
    (modifiers, key)
}

/// The same binding in WAI-ARIA `aria-keyshortcuts` spelling ("Meta+Shift+A"), chords separated
/// by a space.
pub fn aria(binding: &KeyBinding) -> SharedString {
    binding
        .keystrokes()
        .iter()
        .map(|stroke| aria_stroke(stroke.modifiers(), stroke.key()))
        .collect::<Vec<_>>()
        .join(" ")
        .into()
}

/// A stored combo in `aria-keyshortcuts` spelling (see [`combo_text`]).
pub fn aria_combo(combo: &str) -> SharedString {
    aria_combo_on(cfg!(target_os = "macos"), combo).into()
}

/// [`aria_combo`] for an explicit platform.
pub fn aria_combo_on(mac: bool, combo: &str) -> String {
    combo
        .split_whitespace()
        .map(|chord| {
            let (modifiers, key) = parse_chord(mac, chord);
            aria_stroke(&modifiers, key)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `aria-keyshortcuts` of the binding `action` has now, resolved through the keymap alone
/// (no `Window` at render): the last context-free binding, else the last of any context.
pub fn aria_for_action(action: &dyn Action, cx: &App) -> Option<SharedString> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let bindings: Vec<&KeyBinding> = keymap.bindings_for_action(action).collect();
    bindings
        .iter()
        .rev()
        .find(|binding| binding.predicate().is_none())
        .or_else(|| bindings.last())
        .map(|binding| aria(binding))
}

/// One stroke in `aria-keyshortcuts` spelling: modifiers in the Mac's ⌃⌥⇧⌘ order, then the key
/// by its DOM `key` name.
fn aria_stroke(modifiers: &Modifiers, key: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (on, word) in [
        (modifiers.control, "Control"),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
        (modifiers.platform, "Meta"),
    ] {
        if on {
            parts.push(word.to_owned());
        }
    }
    parts.push(aria_key(key));
    parts.join("+")
}

/// A key by its DOM `key` name ("ArrowUp", "Escape"); a letter in capitals.
fn aria_key(key: &str) -> String {
    let named = match key {
        "enter" => "Enter",
        "escape" => "Escape",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "tab" => "Tab",
        "space" => "Space",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        other => return other.to_uppercase(),
    };
    named.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(control: bool, alt: bool, shift: bool, platform: bool) -> Modifiers {
        Modifiers { control, alt, shift, platform, function: false }
    }

    #[test]
    fn a_mac_stroke_is_glyphs_in_canonical_order() {
        assert_eq!(stroke_text_on(true, &held(true, true, true, true), "a"), "⌃⌥⇧⌘A");
        assert_eq!(stroke_text_on(true, &held(false, false, true, true), "enter"), "⇧⌘⏎");
        assert_eq!(stroke_text_on(true, &held(false, false, false, false), "escape"), "Esc");
        assert_eq!(stroke_text_on(true, &held(false, false, true, false), "tab"), "⇧⇥");
        assert_eq!(stroke_text_on(true, &held(false, false, false, true), "["), "⌘[");
        assert_eq!(stroke_text_on(true, &held(false, false, false, false), "f5"), "F5");
        assert_eq!(stroke_text_on(true, &held(false, true, false, false), "up"), "⌥↑");
    }

    #[test]
    fn elsewhere_a_stroke_is_words() {
        assert_eq!(stroke_text_on(false, &held(true, false, true, false), "a"), "Ctrl+Shift+A");
        assert_eq!(stroke_text_on(false, &held(false, false, false, false), "escape"), "Esc");
        assert_eq!(stroke_text_on(false, &held(true, false, false, false), "backspace"), "Ctrl+Backspace");
    }

    #[test]
    fn a_stored_combo_reads_like_a_binding() {
        assert_eq!(combo_text_on(true, "mod-shift-a"), "⇧⌘A");
        assert_eq!(combo_text_on(true, "shift-tab"), "⇧⇥");
        assert_eq!(combo_text_on(true, "escape"), "Esc");
        assert_eq!(combo_text_on(true, "ctrl-tab"), "⌃⇥");
        assert_eq!(combo_text_on(true, "mod--"), "⌘-");
        assert_eq!(combo_text_on(true, "mod-k mod-s"), "⌘K\u{2009}⌘S");
        assert_eq!(combo_text_on(false, "mod-shift-a"), "Ctrl+Shift+A");
        assert_eq!(combo_text_on(false, "mod-2"), "Ctrl+2");
    }

    #[test]
    fn aria_spells_dom_key_names() {
        assert_eq!(aria_combo_on(true, "mod-e"), "Meta+E");
        assert_eq!(aria_combo_on(true, "mod-shift-f"), "Shift+Meta+F");
        assert_eq!(aria_combo_on(true, "shift-tab"), "Shift+Tab");
        assert_eq!(aria_combo_on(true, "escape"), "Escape");
        assert_eq!(aria_combo_on(false, "mod-up"), "Control+ArrowUp");
    }

    gpui::actions!(keys_test, [Probe, Contextual, Unbound]);

    #[gpui::test]
    fn an_action_reads_its_binding_now_and_nothing_unbound(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("cmd-e", Probe, None),
                KeyBinding::new("cmd-t", Contextual, Some("Terminal")),
            ]);
        });
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            assert_eq!(for_action(&Probe, None, window, cx), Some(combo_text("cmd-e")));
            assert_eq!(for_action(&Unbound, None, window, cx), None);
            // A binding only a context holds is not offered outside it.
            assert_eq!(for_action(&Contextual, None, window, cx), None);
            assert_eq!(aria_for_action(&Probe, cx).as_deref(), Some("Meta+E"));
        });
        // A rebind shows at the next read.
        window.update(|window, cx| {
            cx.clear_key_bindings();
            cx.bind_keys([KeyBinding::new("cmd-shift-e", Probe, None)]);
            assert_eq!(for_action(&Probe, None, window, cx), Some(combo_text("cmd-shift-e")));
        });
    }
}
